"use strict";

// 无第三方依赖的前端划词回归：在 VM 中运行完整 ui/app.js，仅 mock Tauri 与 DOM。
const assert = require("node:assert/strict");
const fs = require("node:fs");
const path = require("node:path");
const vm = require("node:vm");

const root = path.resolve(__dirname, "..");
const source = fs.readFileSync(path.join(root, "ui", "app.js"), "utf8");

class FakeElement {
  constructor(id) {
    this.id = id;
    this.value = "";
    this.checked = false;
    this.disabled = false;
    this.hidden = false;
    this.textContent = "";
    this.innerHTML = "";
    this.className = "";
    this.title = "";
    this.placeholder = "";
    this.dataset = {};
    this.style = {};
    this.attributes = {};
    this.children = [];
    this.listeners = new Map();
    this.scrollTop = 0;
    this.clientHeight = 100;
    this.classList = {
      add: (...names) => names.forEach((name) => this._addClass(name)),
      remove: (...names) => names.forEach((name) => this._removeClass(name)),
      contains: (name) => this.className.split(/\s+/).includes(name),
      toggle: (name, force) => {
        const on = force === undefined ? !this.classList.contains(name) : !!force;
        if (on) this._addClass(name);
        else this._removeClass(name);
        return on;
      },
    };
  }

  _addClass(name) {
    if (!this.classList.contains(name)) {
      this.className = `${this.className} ${name}`.trim();
    }
  }

  _removeClass(name) {
    this.className = this.className.split(/\s+/).filter((item) => item && item !== name).join(" ");
  }

  addEventListener(type, handler) {
    if (!this.listeners.has(type)) this.listeners.set(type, []);
    this.listeners.get(type).push(handler);
  }

  removeEventListener(type, handler) {
    const handlers = this.listeners.get(type) || [];
    this.listeners.set(type, handlers.filter((item) => item !== handler));
  }

  dispatch(type, extra = {}) {
    const event = { type, target: this, currentTarget: this, preventDefault() {}, ...extra };
    for (const handler of [...(this.listeners.get(type) || [])]) handler(event);
  }

  click() { this.dispatch("click"); }
  focus() { document.activeElement = this; }
  blur() { if (document.activeElement === this) document.activeElement = null; }
  setAttribute(name, value) { this.attributes[name] = String(value); }
  removeAttribute(name) { delete this.attributes[name]; }
  getAttribute(name) { return this.attributes[name] ?? null; }
  appendChild(child) { this.children.push(child); return child; }
  append(...children) { this.children.push(...children); }
  removeChild(child) { this.children = this.children.filter((item) => item !== child); }
  replaceChildren(...children) { this.children = [...children]; }
  remove() {}
  querySelector() { return null; }
  querySelectorAll() { return []; }
  closest() { return null; }
  scrollIntoView() {}
  scrollTo() {}
  getBoundingClientRect() { return { x: 0, y: 0, top: 0, left: 0, right: 600, bottom: 76, width: 600, height: 76 }; }
}

const elements = new Map();
const document = {
  activeElement: null,
  getElementById(id) {
    if (!elements.has(id)) elements.set(id, new FakeElement(id));
    return elements.get(id);
  },
  createElement(tag) { return new FakeElement(tag); },
  querySelector() { return null; },
  querySelectorAll() { return []; },
  addEventListener() {},
  removeEventListener() {},
};

const calls = [];
const sequence = [];
const eventHandlers = new Map();
const pendingSearches = [];
const timers = new Set();
const startupSelection = {
  text: "@库名 +必须 -排除 title:普通 mtime:>7d",
  request_id: 1,
  method: "uia",
  warning: null,
};
let resolveReady;
const ready = new Promise((resolve) => { resolveReady = resolve; });
const listenerFailureMode = process.argv[2] || "none";
let failedListenerAttempts = 0;
let resolveListenerFailure;
const listenerFailureFinished = new Promise((resolve) => { resolveListenerFailure = resolve; });

function trackedTimeout(handler, delay, ...args) {
  const id = setTimeout(() => {
    timers.delete(id);
    handler(...args);
  }, delay);
  timers.add(id);
  return id;
}

function trackedClearTimeout(id) {
  timers.delete(id);
  clearTimeout(id);
}

function emit(name, payload) {
  for (const handler of [...(eventHandlers.get(name) || [])]) handler({ payload });
}

const fixtureConfig = {
  knowledge_dir: "C:/notes",
  knowledge_dirs: ["C:/notes"],
  roots: [{ path: "C:/notes", label: "库名", enabled: true }],
  scope: { exclude_globs: [], include_globs: [], include_exts: ["md"], max_file_kb: 4096, max_age_days: 0 },
  x: null,
  y: null,
  width: 600,
  expanded_h: 600,
  pin_mode: false,
  shortcut: "Alt+Space",
  selection_search_enabled: false,
  selection_shortcut: "Alt+Shift+Space",
  selection_clipboard_fallback: false,
  search_history: [],
  presets: [],
  git_sources: [],
  webdav_sources: [],
  scope_usage: {},
  ai: { enabled: false, base_url: "", api_key: "", model: "", timeout_ms: 20000, top_n: 20 },
  api: { enabled: false, port: 18230, token: "" },
};

async function invoke(command, args = {}) {
  calls.push({ command, args });
  switch (command) {
    case "get_config":
      return structuredClone(fixtureConfig);
    case "get_search_history":
      return [];
    case "rescan":
      return { docs: 0, chunks: 0, updated: 0, removed: 0, duration_ms: 0 };
    case "health_check":
      return [];
    case "search":
      return new Promise((resolve) => pendingSearches.push({ args, resolve }));
    case "search_apps":
      return [];
    case "add_search_history":
      return [args.q];
    case "save_config":
      return args.cfg;
    case "save_synonyms":
    case "save_stopwords":
      return true;
    case "frontend_ready":
      sequence.push("frontend_ready");
      // 模拟后端在收到就绪信号后立即放行启动期间排队的划词事件。
      emit("selection-search", startupSelection);
      resolveReady();
      return null;
    default:
      return null;
  }
}

const storage = new Map();
const sandbox = {
  console: { log() {}, warn() {}, error() {} },
  document,
  navigator: { clipboard: { writeText: async () => {} } },
  localStorage: {
    getItem: (key) => storage.get(key) ?? null,
    setItem: (key, value) => storage.set(key, String(value)),
    removeItem: (key) => storage.delete(key),
  },
  setTimeout: trackedTimeout,
  clearTimeout: trackedClearTimeout,
  setInterval,
  clearInterval,
  requestAnimationFrame: (handler) => trackedTimeout(handler, 0),
  cancelAnimationFrame: trackedClearTimeout,
  __TAURI__: {
    core: { invoke },
    event: {
      listen(name, handler) {
        sequence.push(`listen:${name}`);
        const shouldFail = name === "selection-search" && (
          listenerFailureMode === "always" ||
          (listenerFailureMode === "once" && failedListenerAttempts === 0)
        );
        if (shouldFail) {
          failedListenerAttempts += 1;
          if (listenerFailureMode === "always" && failedListenerAttempts === 3) {
            resolveListenerFailure();
          }
          return Promise.reject(new Error("模拟监听注册失败"));
        }
        if (!eventHandlers.has(name)) eventHandlers.set(name, []);
        eventHandlers.get(name).push(handler);
        return Promise.resolve(() => {});
      },
    },
  },
};
sandbox.window = sandbox;
sandbox.globalThis = sandbox;
sandbox.addEventListener = () => {};
sandbox.removeEventListener = () => {};

const context = vm.createContext(sandbox);
vm.runInContext(source, context, { filename: "ui/app.js" });
vm.runInContext(`
  globalThis.__selectionTest = {
    applySelectionSearch,
    doSearch,
    clearInput,
    setSelectionFilters: (type, root) => { typeFilter = type; rootFilter = root; },
    getState: () => ({ searchSeq, selectionQueryActive, hits, appHits, lastQuery })
  };
`, context);

const flush = () => new Promise((resolve) => setImmediate(resolve));
const emptyResponse = { hits: [], terms: [], duration_ms: 1 };

async function settleNextSearch(response = emptyResponse) {
  const pending = pendingSearches.shift();
  assert.ok(pending, "应发起一次搜索请求");
  pending.resolve(response);
  await flush();
  await flush();
}

(async () => {
  if (listenerFailureMode === "always") {
    await listenerFailureFinished;
    await flush();
    await flush();
    assert.equal(failedListenerAttempts, 3, "关键监听器应重试 3 次");
    assert.equal(calls.some((item) => item.command === "frontend_ready"), false,
      "监听失败时不得放行后端事件队列");
    const banner = document.getElementById("app-banner");
    assert.equal(banner.hidden, false);
    assert.ok(banner.textContent.includes("启动事件监听失败"));
    for (const id of timers) clearTimeout(id);
    timers.clear();
    console.log("SELECTION_UI_LISTENER_FAILURE_OK 重试3次/不发ready/可见提示");
    return;
  }

  await ready;
  await flush();
  if (listenerFailureMode === "once") assert.equal(failedListenerAttempts, 1, "首次失败后应重试成功");

  const readyIndex = sequence.indexOf("frontend_ready");
  for (const name of ["selection-search", "selection-search-error", "app-warning", "ui-reset"]) {
    assert.ok(sequence.indexOf(`listen:${name}`) < readyIndex, `${name} 必须先于 frontend_ready 注册`);
  }
  assert.equal(document.getElementById("set-selection-enabled").checked, false);
  assert.equal(document.getElementById("set-selection-fallback").disabled, true);

  // frontend_ready 期间放行的排队划词：监听器必须已经可用。
  const selected = startupSelection.text;
  const q = document.getElementById("q");
  const resList = document.getElementById("res-list");
  const test = sandbox.__selectionTest;
  assert.equal(q.value, selected);
  assert.equal(test.getState().selectionQueryActive, true);
  const literalCall = calls.find((item) => item.command === "search");
  assert.equal(literalCall.args.q, selected);
  assert.equal(literalCall.args.literal, true);
  assert.equal(literalCall.args.rootFilter, null, "@库名 不得被解释为范围前缀");
  assert.equal(literalCall.args.typeFilter, null);
  assert.equal(calls.some((item) => item.command === "add_search_history"), false);
  await settleNextSearch();
  assert.equal(calls.some((item) => item.command === "add_search_history"), false);

  // 已选中的类型/库 chips 继续生效，但选区自身的前缀仍按字面量传递。
  test.setSelectionFilters("Law", "库名");
  emit("selection-search", { text: "+必须 root:另一库", request_id: 2, method: "uia", warning: null });
  const scopedCall = calls.filter((item) => item.command === "search").at(-1);
  assert.equal(scopedCall.args.q, "+必须 root:另一库");
  assert.equal(scopedCall.args.literal, true);
  assert.equal(scopedCall.args.rootFilter, "库名");
  assert.equal(scopedCall.args.typeFilter, "Law");
  await settleNextSearch();
  assert.equal(calls.some((item) => item.command === "add_search_history"), false);
  const beforeDuplicate = calls.filter((item) => item.command === "search").length;
  emit("selection-search", { text: "过期选区", request_id: 1, method: "uia", warning: null });
  await flush();
  assert.equal(calls.filter((item) => item.command === "search").length, beforeDuplicate,
    "旧请求号不得替换最新划词结果");
  assert.equal(q.value, "+必须 root:另一库");
  test.setSelectionFilters("all", "all");

  // IME 合成期间不搜索，选词结束后再触发普通搜索。
  const beforeComposition = calls.filter((item) => item.command === "search").length;
  q.value = "中文";
  q.dispatch("compositionstart");
  q.dispatch("input");
  assert.equal(test.getState().selectionQueryActive, false);
  await flush();
  assert.equal(calls.filter((item) => item.command === "search").length, beforeComposition);
  q.dispatch("compositionend");
  await new Promise((resolve) => setTimeout(resolve, 70));
  await settleNextSearch();

  // 采集失败事件也必须通过可见提示条反馈。
  emit("selection-search-error", "未检测到可读取的选区");
  const banner = document.getElementById("app-banner");
  assert.equal(banner.hidden, false);
  assert.equal(banner.textContent, "未检测到可读取的选区");

  // 普通输入立即退出划词模式，恢复高级语法并记录历史。
  q.value = "@法律 劳动合同";
  q.dispatch("input");
  assert.equal(test.getState().selectionQueryActive, false);
  const normalSearch = test.doSearch();
  const normalCall = calls.filter((item) => item.command === "search").at(-1);
  assert.equal(normalCall.args.q, "劳动合同");
  assert.equal(normalCall.args.literal, false);
  assert.equal(normalCall.args.typeFilter, "Law");
  await settleNextSearch();
  await normalSearch;
  assert.ok(calls.some((item) => item.command === "add_search_history" && item.args.q === "劳动合同"));

  // ui-reset/clear 后，旧请求即使迟到也不能回写结果。
  q.value = "新查询";
  const staleSearch = test.doSearch();
  emit("ui-reset");
  assert.equal(q.value, "");
  assert.equal(test.getState().selectionQueryActive, false);
  await settleNextSearch({
    hits: [{
      path: "stale.md", title: "迟到结果", breadcrumbs: [], doc_type: "Note",
      snippet: "迟到结果", line: 1, root: "库名", abs: "C:/notes/stale.md", mtime: 0, score: 1,
    }],
    terms: ["迟到结果"],
    duration_ms: 1,
  });
  await staleSearch;
  assert.equal(test.getState().hits.length, 0);
  assert.equal(resList.innerHTML.includes("迟到结果"), false);

  // 设置保存三个划词字段；关闭功能时强制关闭 Ctrl+C 回退。
  const enabled = document.getElementById("set-selection-enabled");
  const shortcut = document.getElementById("set-selection-shortcut");
  const fallback = document.getElementById("set-selection-fallback");
  const apply = document.getElementById("btn-apply");
  enabled.checked = true;
  shortcut.value = "Alt+Shift+S";
  fallback.checked = true;
  await apply.listeners.get("click")[0]();
  const saved = calls.filter((item) => item.command === "save_config").at(-1);
  assert.equal(saved.args.cfg.selection_search_enabled, true);
  assert.equal(saved.args.cfg.selection_shortcut, "Alt+Shift+S");
  assert.equal(saved.args.cfg.selection_clipboard_fallback, true);

  enabled.checked = false;
  fallback.checked = true;
  await apply.listeners.get("click")[0]();
  const disabledSave = calls.filter((item) => item.command === "save_config").at(-1);
  assert.equal(disabledSave.args.cfg.selection_search_enabled, false);
  assert.equal(disabledSave.args.cfg.selection_clipboard_fallback, false);

  test.clearInput();
  for (const id of timers) clearTimeout(id);
  timers.clear();
  console.log("SELECTION_UI_OK 字面量/历史/竞态/启动监听/设置保存");
})().catch((error) => {
  for (const id of timers) clearTimeout(id);
  console.error(error);
  process.exitCode = 1;
});
