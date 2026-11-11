/* 知识悬浮搜索 —— 渲染进程（通过 withGlobalTauri 注入的 window.__TAURI__ 与主进程通信）
   形态逻辑：折叠=仅搜索条；有输入/打开设置时展开窗口并显示对应面板 */

const { invoke } = window.__TAURI__.core;
const { listen } = window.__TAURI__.event;

/* 全局错误兜底：任何未捕获异常直接显示在界面上（透明窗上红条），避免静默黑屏 */
window.addEventListener("error", (e) => {
  const el = document.getElementById("err-banner");
  if (!el) return;
  el.textContent = "脚本错误: " + (e.message || "未知");
  el.hidden = false;
});
window.addEventListener("unhandledrejection", (e) => {
  console.error(e.reason);
});

const $q = document.getElementById("q");
const $bar = document.getElementById("bar");
const $btnClear = document.getElementById("btn-clear");
const $results = document.getElementById("results");
const $chipsType = document.getElementById("chips-type");
const $chipsRoot = document.getElementById("chips-root");
const $resList = document.getElementById("res-list");
const $stats = document.getElementById("stats");
const $resCount = document.getElementById("res-count");
const $settings = document.getElementById("settings");
const $settingsTabs = document.getElementById("settings-tabs");
const $rootsList = document.getElementById("roots-list");
const $setExcludes = document.getElementById("set-excludes");
const $setExts = document.getElementById("set-exts");
const $setMaxkb = document.getElementById("set-maxkb");
const $setShortcut = document.getElementById("set-shortcut");
const $setWidth = document.getElementById("set-width");
const $setPin = document.getElementById("set-pin");
const $setSynonyms = document.getElementById("set-synonyms");
const $setStopwords = document.getElementById("set-stopwords");
const $historyPanel = document.getElementById("history-panel");
const $historyList = document.getElementById("history-list");

let hits = [];            // 当前搜索结果（全量，范围已下沉引擎）
let searchHistory = [];   // 搜索历史
let active = -1;          // 键盘选中的位置（visible 内下标）
let debounceTimer = null;
let cfg = null;           // 主进程下发的配置
let settingsOpen = false; // 设置面板展开状态
let settingsTab = "scope";
let expanded = false;     // 当前窗口形态（与后端同步）
let lastTerms = [];       // 实际参与匹配的词（含同义词展开），用于高亮
let lastQuery = "";       // 剥离 @前缀 后的实际检索词（高亮/展开复渲用）
let ctxCache = {};        // 命中项上下文缓存: 原始下标 -> string
let ctxOpen = {};         // 命中项上下文展开状态
let typeFilter = "all";   // 类型范围：all | Law | Manual | QA | Note（下沉引擎）
let rootFilter = "all";   // 库范围：all | 根显示名（下沉引擎）
let visible = [];         // 过滤后的可见列表 [{h, i}]（i 为原始下标）
let searchLimit = 50;     // 当前搜索结果上限（分页递增）

const TYPE_LABEL = { Law: "法律", Manual: "文档", QA: "问答", Note: "笔记" };
const TYPE_ALIAS = {
  law: "Law", 法律: "Law", legal: "Law",
  manual: "Manual", 文档: "Manual", doc: "Manual",
  qa: "QA", 问答: "QA",
  note: "Note", 笔记: "Note",
  all: "all", 全部: "all",
};

/* ---------- 工具 ---------- */
function escapeHtml(s) {
  return s
    .replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;")
    .replace(/"/g, "&quot;").replace(/'/g, "&#39;");
}
function escapeReg(s) {
  return s.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
}

function rootLabelOf(r) {
  return (r.label && r.label.trim()) || r.path.split(/[\\/]/).filter(Boolean).pop() || r.path;
}

/** 把片段/标题中的命中词包上 <mark>（大小写不敏感，先转义再匹配；含同义词） */
function highlight(snippet, query) {
  const safe = escapeHtml(snippet);
  const fromQuery = (query || "").trim().split(/\s+/).filter(Boolean);
  const terms = [...fromQuery, ...lastTerms].map(escapeReg);
  if (!terms.length) return escapeHtml(snippet);
  // 按长度降序匹配，避免短词破坏长词高亮
  terms.sort((a, b) => b.length - a.length);
  const re = new RegExp("(" + [...new Set(terms)].join("|") + ")", "gi");
  return safe.replace(re, "<mark>$1</mark>");
}

function shortPath(p) {
  const parts = p.split("/");
  return parts.length > 2 ? "…/" + parts.slice(-2).join("/") : p;
}

/* ---------- 窗口形态 ---------- */
function setExpanded(on) {
  if (expanded === on) return;
  expanded = on;
  invoke("ui_mode", { mode: on ? "expanded" : "collapsed" }).catch(() => {});
}

/** 钉住模式下禁用拖动（移除拖动区属性） */
function applyPin() {
  if (!cfg) return;
  if (cfg.pin_mode) {
    $bar.removeAttribute("data-tauri-drag-region");
  } else {
    $bar.setAttribute("data-tauri-drag-region", "");
  }
}

/* ---------- 范围（chips + @前缀） ---------- */
/** @前缀解析：@法律 / @库名 → 范围；命中则剥离前缀 */
function parseScopePrefix(raw) {
  const m = raw.match(/^@(\S+)\s*/);
  if (!m) return { q: raw, typeF: typeFilter, rootF: rootFilter, hit: false };
  const tag = m[1].toLowerCase();
  let typeF = typeFilter, rootF = rootFilter, hit = false;
  if (tag in TYPE_ALIAS) {
    typeF = TYPE_ALIAS[tag];
    hit = true;
  } else {
    const roots = (cfg && cfg.roots ? cfg.roots : []).filter((r) => r.enabled);
    const found = roots.find((r) => rootLabelOf(r).toLowerCase().includes(tag));
    if (found) {
      rootF = rootLabelOf(found);
      hit = true;
    }
  }
  return { q: raw.slice(m[0].length), typeF, rootF, hit };
}

/** 依据 cfg 重建根 chips */
function renderRootChips() {
  const roots = (cfg && cfg.roots ? cfg.roots : []).filter((r) => r.enabled);
  const btns = ['<button class="chip' + (rootFilter === "all" ? " active" : "") + '" data-f="all">全部</button>']
    .concat(roots.map((r) => {
      const label = rootLabelOf(r);
      return `<button class="chip${rootFilter === label ? " active" : ""}" data-f="${escapeHtml(label)}">${escapeHtml(label)}</button>`;
    }));
  $chipsRoot.innerHTML = btns.join("");
}

function setChipActive($wrap, f) {
  $wrap.querySelectorAll(".chip").forEach((c) => c.classList.toggle("active", c.dataset.f === f));
}

/* ---------- 搜索历史 ---------- */
async function recordSearch(q) {
  if (!q.trim()) return;
  try {
    searchHistory = await invoke("add_search_history", { q });
  } catch (_) {}
}

function renderHistory() {
  if (!searchHistory.length) {
    $historyList.innerHTML = '<div class="empty-sm">暂无搜索记录</div>';
    return;
  }
  $historyList.innerHTML = searchHistory
    .map((q) => `
      <div class="history-item" data-q="${escapeHtml(q)}">
        <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><circle cx="11" cy="11" r="8"/><path d="m21 21-4.35-4.35"/></svg>
        <span>${escapeHtml(q)}</span>
      </div>`)
    .join("");
  $historyList.querySelectorAll(".history-item").forEach((el) => {
    el.addEventListener("click", () => {
      $q.value = el.dataset.q;
      $btnClear.hidden = false;
      $historyPanel.hidden = true;
      doSearch();
      $q.focus();
    });
  });
}

async function loadHistory() {
  try {
    searchHistory = await invoke("get_search_history");
  } catch (_) { searchHistory = []; }
}


function renderCtx(data, origIdx) {
  if (!data) return '<div class="hit-ctx">加载中…</div>';
  if (typeof data === "string") return `<div class="hit-ctx">${escapeHtml(data)}</div>`;
  const h = hits[origIdx];
  if (!h) return `<div class="hit-ctx">${escapeHtml(JSON.stringify(data))}</div>`;
  const lines = data.content.split("\n");
  const html = lines.map((line, idx) => {
    const lineNo = data.start_line + idx;
    const isMatch = lineNo >= h.line && lineNo < h.line + 1;
    return `<div class="ctx-line${isMatch ? " ctx-match" : ""}"><span class="ctx-ln">${lineNo}</span><span class="ctx-text">${escapeHtml(line)}</span></div>`;
  }).join("");
  return `<div class="hit-ctx">${html}</div>`;
}


/* ---------- 范围预设 ---------- */
function renderPresetSelect() {
  const $sel = document.getElementById("preset-select");
  const presets = (cfg && cfg.presets) || [];
  $sel.innerHTML = '<option value="">选择预设…</option>' +
    presets.map((p) => `<option value="${escapeHtml(p.name)}">${escapeHtml(p.name)}</option>`).join("");
}

async function applyPreset(name) {
  if (!name) return;
  try {
    cfg = await invoke("load_scope_preset", { name });
    applyPin();
    renderRootsEditor();
    renderRootChips();
    $setExcludes.value = (cfg.scope.exclude_globs || []).join("\n");
    $setExts.value = (cfg.scope.include_exts || ["md","markdown","txt"]).join(", ");
    $setMaxkb.value = cfg.scope.max_file_kb || 4096;
    const s = await invoke("rescan");
    $stats.textContent = `${s.docs} 个文档 · ${s.chunks} 个知识块`;
    doSearch();
  } catch (e) {
    $stats.textContent = "加载预设失败: " + e;
  }
}

/* ---------- 渲染 ---------- */
function render(query) {
  visible = filteredHits();
  $resCount.textContent = visible.length ? `${visible.length} 条结果` : "";
  if (!visible.length) {
    const anyHits = hits.length > 0;
    $resList.innerHTML =
      '<div class="empty">' +
      (anyHits
        ? "当前范围下没有结果，试试调整上方筛选或 @范围"
        : query
          ? "没有找到相关内容"
          : "输入关键词，检索你的本地知识库") +
      "</div>";
    return;
  }
  $resList.innerHTML = visible
    .map(({ h, i }, pos) => {
      const bc = (h.breadcrumbs && h.breadcrumbs.length)
        ? `<div class="hit-bc">${h.breadcrumbs.map((b) => escapeHtml(b)).join('<span class="bc-sep">›</span>')}</div>`
        : "";
      const badge = TYPE_LABEL[h.doc_type]
        ? `<span class="badge badge-${h.doc_type.toLowerCase()}">${TYPE_LABEL[h.doc_type]}</span>`
        : "";
      const rootTag = h.root
        ? `<span class="hit-root">${escapeHtml(h.root)}</span>`
        : "";
      const open = ctxOpen[i];
      const ctx = open
        ? renderCtx(ctxCache[i], i)
        : "";
      return `
      <div class="hit ${pos === active ? "active" : ""}" data-pos="${pos}">
        <div class="hit-top">
          ${badge}
          <span class="hit-title">${highlight(h.title, query)}</span>
          ${rootTag}
          <span class="hit-path">${escapeHtml(shortPath(h.path))}</span>
          <button class="ctx-btn" data-ctx="${i}" title="展开上下文">
            <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="m7 6 5 5 5-5"/><path d="m7 13 5 5 5-5"/></svg>
          </button>
        </div>
        ${bc}
        <div class="hit-snippet">${highlight(h.snippet, query)}</div>
        ${ctx}
      </div>`;
    })
    .join("");

  $resList.querySelectorAll(".hit").forEach((el) => {
    el.addEventListener("click", () => openHit(Number(el.dataset.pos), false));
    el.addEventListener("dblclick", () => openHit(Number(el.dataset.pos), true));
    el.addEventListener("mousemove", () => setActive(Number(el.dataset.pos)));
  });
  // 上下文展开按钮（阻止冒泡，不触发打开文件）
  // 加载更多按钮
  const moreBtn = $resList.querySelector(".load-more-btn");
  if (moreBtn) {
    moreBtn.addEventListener("click", (e) => {
      e.stopPropagation();
      searchLimit += 50;
      doSearch();
    });
  }
  $resList.querySelectorAll(".ctx-btn").forEach((btn) => {
    btn.addEventListener("click", (e) => {
      e.stopPropagation();
      toggleContext(Number(btn.dataset.ctx));
    });
  });
}

/** 就地展开上下文：请求源文件 前N行+后M行（不打开文件） */
async function toggleContext(origIdx) {
  const item = visible.find((x) => x.i === origIdx);
  const h = item ? item.h : hits[origIdx];
  if (!h) return;
  if (ctxOpen[origIdx]) {
    delete ctxOpen[origIdx];
  } else {
    ctxOpen[origIdx] = true;
    if (!ctxCache[origIdx]) {
      try {
        ctxCache[origIdx] = await invoke("expand_context", {
          path: h.abs, line: h.line, before: 10, after: 20,
        });
      } catch (e) {
        ctxCache[origIdx] = "读取上下文失败: " + e;
      }
    }
  }
  render(lastQuery);
  const el = $resList.querySelector(`.ctx-btn[data-ctx="${origIdx}"]`);
  if (el) el.closest(".hit").scrollIntoView({ block: "nearest" });
}

function setActive(pos) {
  if (pos === active) return;
  active = pos;
  $resList.querySelectorAll(".hit").forEach((el, idx) => {
    el.classList.toggle("active", idx === pos);
  });
  const el = $resList.querySelector(`.hit[data-pos="${pos}"]`);
  if (el) el.scrollIntoView({ block: "nearest" });
}

/** 类型过滤后的可见列表（类型 chips 已下沉引擎，此处仅保留结构） */
function filteredHits() {
  return hits.map((h, i) => ({ h, i }));
}

/** 根据输入与面板状态刷新：面板显隐 + 窗口展开/折叠 */
function updateView() {
  const query = lastQuery;
  const wantExpand = $q.value.trim().length > 0 || settingsOpen;
  setExpanded(wantExpand);
  $results.hidden = $q.value.trim().length === 0;
  $settings.hidden = !settingsOpen;
  render(query);
}

/* ---------- 动作 ---------- */
async function doSearch() {
  const raw = $q.value;
  if (!raw.trim()) {
    hits = [];
    active = -1;
    lastTerms = [];
    lastQuery = "";
    ctxCache = {};
    ctxOpen = {};
    updateView();
    return;
  }
  const parsed = parseScopePrefix(raw);
  lastQuery = parsed.q;
  let r;
  try {
    r = await invoke("search", {
      q: parsed.q,
      typeFilter: parsed.typeF === "all" ? null : parsed.typeF,
      rootFilter: parsed.rootF === "all" ? null : parsed.rootF,
      limit: searchLimit,
    });
    hits = r.hits;
    lastTerms = r.terms || [];
  } catch (e) {
    hits = [];
    lastTerms = [];
    console.error(e);
  }
  // 新结果集：上下文缓存必须清空，否则命中序号错位会展示旧内容
  ctxCache = {};
  ctxOpen = {};
  active = hits.length ? 0 : -1;
  render(parsed.q);
  if (r.duration_ms) {
    $resCount.textContent = `${visible.length} 条结果 · ${r.duration_ms}ms`;
  }
  // If we got exactly the limit, there might be more results
  if (hits.length >= searchLimit) {
    const btn = document.createElement("button");
    btn.className = "load-more-btn";
    btn.textContent = "加载更多…";
    btn.style.cssText = "display:block;width:100%;padding:8px;border:none;background:var(--btn-bg);color:var(--accent);cursor:pointer;font-size:12px;border-radius:6px;margin-top:4px;";
    btn.addEventListener("click", () => { searchLimit += 50; doSearch(); });
    $resList.appendChild(btn);
  }
  recordSearch(parsed.q);
}

function clearInput() {
  $q.value = "";
  $btnClear.hidden = true;
  hits = [];
  active = -1;
  lastTerms = [];
  lastQuery = "";
  ctxCache = {};
  ctxOpen = {};
  updateView();
}

function openHit(pos, reveal) {
  const item = visible[pos];
  if (!item) return;
  invoke(reveal ? "reveal_file" : "open_file", { path: item.h.abs }).catch((e) =>
    console.error("打开失败:", e)
  );
}

/* ---------- 设置面板 ---------- */
/** 范围页：根列表编辑器 */
function renderRootsEditor() {
  const roots = cfg && cfg.roots ? cfg.roots : [];
  $rootsList.innerHTML = roots.length
    ? roots
        .map((r, i) => {
          const label = rootLabelOf(r);
          return `
      <div class="root-row" data-i="${i}">
        <input class="set-input root-label" data-k="label" value="${escapeHtml(r.label)}" placeholder="显示名" spellcheck="false" />
        <span class="root-path" title="${escapeHtml(r.path)}">${escapeHtml(shortPath(r.path))}</span>
        <label class="root-en"><input type="checkbox" data-k="enabled" ${r.enabled ? "checked" : ""} /> 启用</label>
        <button class="root-del" data-del="${i}">移除</button>
      </div>`;
        })
        .join("")
    : '<div class="empty-sm">尚未添加目录，点击下方按钮选择</div>';
}

function openSettings() {
  settingsOpen = true;
  renderPresetSelect();
  settingsTab = "scope";
  activateTab("scope");
  if (cfg) {
    $setExcludes.value = (cfg.scope && cfg.scope.exclude_globs ? cfg.scope.exclude_globs : []).join("\n");
    $setExts.value = (cfg.scope && cfg.scope.include_exts ? cfg.scope.include_exts : ["md", "markdown", "txt"]).join(", ");
    $setMaxkb.value = (cfg.scope && cfg.scope.max_file_kb) || 4096;
    $setShortcut.value = cfg.shortcut;
    $setWidth.value = cfg.width;
    $setPin.checked = cfg.pin_mode;
  }
  renderRootsEditor();
  renderStats();
  invoke("get_stopwords")
    .then((s) => { $setStopwords.value = s; })
    .catch(() => { $setStopwords.value = "[]"; });
  invoke("get_synonyms")
    .then((s) => { $setSynonyms.value = s; })
    .catch(() => { $setSynonyms.value = "{}"; });
  updateView();
}
function closeSettings() {
  settingsOpen = false;
  updateView();
}

function activateTab(name) {
  settingsTab = name;
  $settingsTabs.querySelectorAll(".tab").forEach((t) => t.classList.toggle("active", t.dataset.tab === name));
  $settings.querySelectorAll(".tab-pane").forEach((p) => {
    p.hidden = p.id !== "tab-" + name;
  });
}

/* ---------- 事件绑定 ---------- */
/* 自动搜索：输入即搜（150ms 防抖）；中文输入法合成期间不触发，选词完成立即搜 */
let composing = false;
$q.addEventListener("compositionstart", () => { composing = true; });
$q.addEventListener("compositionend", () => {
  composing = false;
  clearTimeout(debounceTimer);
  debounceTimer = setTimeout(doSearch, 50);
});
$q.addEventListener("input", () => {
  $btnClear.hidden = !$q.value;
  $historyPanel.hidden = !!$q.value.trim();
  if (composing) return;
  clearTimeout(debounceTimer);
  debounceTimer = setTimeout(doSearch, 150);
});

$q.addEventListener("focus", () => {
  if (!$q.value.trim() && searchHistory.length) {
    renderHistory();
    $historyPanel.hidden = false;
  }
});

$q.addEventListener("blur", () => {
  // 延迟隐藏以允许点击历史项
  setTimeout(() => { $historyPanel.hidden = true; }, 200);
});

$q.addEventListener("keydown", (e) => {
  if (e.key === "ArrowDown") {
    e.preventDefault();
    if (visible.length) setActive(Math.min(active + 1, visible.length - 1));
  } else if (e.key === "ArrowUp") {
    e.preventDefault();
    if (visible.length) setActive(Math.max(active - 1, 0));
  } else if (e.key === "Enter") {
    if (composing) return;
    clearTimeout(debounceTimer);
    doSearch();
    if (visible.length) openHit(active < 0 ? 0 : active, e.ctrlKey);
  } else if (e.key === "Escape") {
    // 优先关设置 → 清空输入 → 隐藏（钉住模式不隐藏）
    if (settingsOpen) {
      closeSettings();
    } else if ($q.value) {
      clearInput();
    } else if (cfg && !cfg.pin_mode) {
      invoke("hide_window");
    }
  }
});

$btnClear.addEventListener("click", () => {
  clearInput();
  $q.focus();
});

document.getElementById("btn-settings").addEventListener("click", () => {
  settingsOpen ? closeSettings() : openSettings();
});

/* 设置 Tabs */
$settingsTabs.addEventListener("click", (e) => {
  const btn = e.target.closest(".tab");
  if (btn) activateTab(btn.dataset.tab);
});

/* 类型 chips（下沉引擎的范围过滤） */
$chipsType.addEventListener("click", (e) => {
  const btn = e.target.closest(".chip");
  if (!btn) return;
  typeFilter = btn.dataset.f;
  setChipActive($chipsType, typeFilter);
  doSearch();
});

/* 库 chips（动态生成，下沉引擎） */
$chipsRoot.addEventListener("click", (e) => {
  const btn = e.target.closest(".chip");
  if (!btn) return;
  rootFilter = btn.dataset.f;
  setChipActive($chipsRoot, rootFilter);
  doSearch();
});


/* 范围预设事件 */
document.getElementById("preset-select").addEventListener("change", (e) => {
  if (e.target.value) applyPreset(e.target.value);
});

/* 存为预设：WebView2 无 prompt()，用内联输入框命名（Enter 确认 / Esc 取消） */
const $presetName = document.getElementById("preset-name-input");
function togglePresetName(show) {
  $presetName.style.display = show ? "" : "none";
  $presetName.value = "";
  if (show) $presetName.focus();
}
document.getElementById("btn-save-preset").addEventListener("click", () => togglePresetName(true));
$presetName.addEventListener("keydown", async (e) => {
  if (e.key !== "Enter") return;
  const name = $presetName.value.trim();
  if (!name) return;
  togglePresetName(false);
  try {
    await invoke("save_scope_preset", { name });
    renderPresetSelect();
    document.getElementById("preset-select").value = name;
  } catch (err) {
    $stats.textContent = "保存预设失败: " + err;
  }
});
$presetName.addEventListener("blur", () => togglePresetName(false));

document.getElementById("btn-del-preset").addEventListener("click", async () => {
  const sel = document.getElementById("preset-select");
  const name = sel.value;
  if (!name) return;
  await invoke("delete_scope_preset", { name });
  renderPresetSelect();
});
/* 选择并添加目录（追加语义，多库混检） */
document.getElementById("btn-pick").addEventListener("click", async () => {
  $stats.textContent = "扫描中…";
  const r = await invoke("pick_folder").catch((e) => ({ error: String(e) }));
  if (r.error) {
    $stats.textContent = "扫描失败: " + r.error;
  } else if (r.dir) {
    cfg = await invoke("get_config");
    renderRootsEditor();
    renderRootChips();
    if (r.stats) $stats.textContent = `${r.stats.docs} 个文档 · ${r.stats.chunks} 个知识块`;
  } else {
    $stats.textContent = "就绪";
  }
});

const $btnRescan = document.getElementById("btn-rescan");
if ($btnRescan) $btnRescan.addEventListener("click", async () => {
  $stats.textContent = "扫描中…";
  try {
    const s = await invoke("rescan");
    $stats.textContent = `${s.docs} 个文档 · ${s.chunks} 个知识块`;
    renderStats();
  } catch (e) {
    $stats.textContent = "扫描失败: " + e;
  }
});

document.getElementById("btn-apply").addEventListener("click", async () => {
  if (!cfg) return;
  try {
    // 1. 同义词词典（JSON 校验 + 热重载）
    const synText = $setSynonyms.value.trim() || "{}";
    await invoke("save_synonyms", { text: synText });
    const swText = $setStopwords.value.trim() || "[]";
    await invoke("save_stopwords", { text: swText });
    // 2. 范围：根列表（编辑器已实时改 cfg.roots）+ 排除/扩展名/上限
    cfg.scope = {
      exclude_globs: $setExcludes.value.split(/\r?\n/).map((s) => s.trim()).filter(Boolean),
      include_exts: $setExts.value.split(/[,\s]+/).map((s) => s.trim().toLowerCase().replace(/^\./, "")).filter(Boolean),
      max_file_kb: Number($setMaxkb.value) || 4096,
    };
    const dirs = cfg.roots.map((r) => r.path);
    const next = {
      ...cfg,
      knowledge_dirs: dirs,
      knowledge_dir: dirs[0] ?? null,
      shortcut: $setShortcut.value.trim() || cfg.shortcut,
      width: Number($setWidth.value) || cfg.width,
      pin_mode: $setPin.checked,
    };
    cfg = await invoke("save_config", { cfg: next });
    applyPin();
    renderRootChips();
    // 3. 范围可能变化：重新同步 + 宽度立即生效
    const s = await invoke("rescan");
    $stats.textContent = `${s.docs} 个文档 · ${s.chunks} 个知识块`;
    expanded = !expanded;
    setExpanded(!expanded);
    closeSettings();
    $q.focus();
  } catch (e) {
    $stats.textContent = "保存失败: " + e;
  }
});

document.getElementById("btn-quit").addEventListener("click", () => invoke("quit_app"));

/* 清空搜索历史 */
document.getElementById("btn-clear-history").addEventListener("click", async () => {
  await invoke("clear_search_history");
  searchHistory = [];
  renderHistory();
});

/* 根列表编辑器（事件委托） */
$rootsList.addEventListener("input", (e) => {
  const row = e.target.closest(".root-row");
  if (!row || !cfg) return;
  const i = Number(row.dataset.i);
  if (e.target.dataset.k === "label") {
    cfg.roots[i].label = e.target.value;
    renderRootChips();
  }
});
$rootsList.addEventListener("change", (e) => {
  const row = e.target.closest(".root-row");
  if (!row || !cfg) return;
  const i = Number(row.dataset.i);
  if (e.target.dataset.k === "enabled") {
    cfg.roots[i].enabled = e.target.checked;
  }
});
$rootsList.addEventListener("click", (e) => {
  const del = e.target.closest(".root-del");
  if (!del || !cfg) return;
  const i = Number(del.dataset.del);
  cfg.roots.splice(i, 1);
  renderRootsEditor();
  renderRootChips();
});

/* 索引变更（文件监听触发）→ 刷新统计并重跑当前搜索 */
listen("index-updated", (ev) => {
  const s = ev.payload;
  $stats.textContent = `${s.docs} 个文档 · ${s.chunks} 个知识块`;
  if ($q.value.trim()) doSearch();
});

/* 后端唤起（快捷键/二次启动/托盘）→ 复位为折叠形态 */
listen("ui-reset", () => {
  $q.value = "";
  $btnClear.hidden = true;
  hits = [];
  active = -1;
  lastTerms = [];
  lastQuery = "";
  ctxCache = {};
  ctxOpen = {};
  settingsOpen = false;
  updateView();
  $q.focus();
});

/* ---------- 初始化 ---------- */
(async () => {
  try {
    cfg = await invoke("get_config");
    $setExcludes.value = (cfg.scope && cfg.scope.exclude_globs ? cfg.scope.exclude_globs : []).join("\n");
    $setExts.value = (cfg.scope && cfg.scope.include_exts ? cfg.scope.include_exts : ["md", "markdown", "txt"]).join(", ");
    $setMaxkb.value = (cfg.scope && cfg.scope.max_file_kb) || 4096;
    $setShortcut.value = cfg.shortcut;
    $setWidth.value = cfg.width;
    $setPin.checked = cfg.pin_mode;
    applyPin();
    renderRootsEditor();
    renderRootChips();
    renderPresetSelect();
    await loadHistory();
    const s = await invoke("rescan");
    $stats.textContent = `${s.docs} 个文档 · ${s.chunks} 个知识块`;
  } catch (e) {
    if (String(e).includes("尚未设置")) {
      $stats.textContent = "请先在设置中添加知识库目录";
      openSettings();
    } else {
      console.error(e);
    }
  }
  $q.focus();
  // 通知主进程：前端就绪，可以显示窗口（避免 webview 未加载完的黑屏/闪块）
  invoke("frontend_ready").catch(() => {});
})();

/* 索引统计仪表盘 */
async function renderStats() {
  const $grid = document.getElementById("index-stats");
  const $typeRow = document.getElementById("stat-type-row");
  const $rootRow = document.getElementById("stat-root-row");
  try {
    const s = await invoke("get_index_stats");
    if (!s.total_docs) { $grid.hidden = true; return; }
    $grid.hidden = false;
    const max = Math.max(...Object.values(s.by_type), 1);
    $typeRow.innerHTML = Object.entries(s.by_type)
      .filter(([,v]) => v > 0)
      .map(([k, v]) => {
        const label = TYPE_LABEL[k] || k;
        const pct = Math.round((v / max) * 100);
        return `<div class="stat-bar-item"><span class="badge badge-${k.toLowerCase()}">${label}</span><div class="stat-bar"><div style="width:${pct}%"></div></div><span class="stat-num">${v}</span></div>`;
      }).join("");
    const roots = Object.entries(s.by_root).filter(([,v]) => v > 0).sort((a,b) => b[1]-a[1]);
    const maxRoot = Math.max(...roots.map(([,v]) => v), 1);
    $rootRow.innerHTML = roots
      .map(([k, v]) => {
        const pct = Math.round((v / maxRoot) * 100);
        return `<div class="stat-bar-item"><span class="stat-lib">${escapeHtml(k)}</span><div class="stat-bar"><div style="width:${pct}%"></div></div><span class="stat-num">${v}</span></div>`;
      }).join("");
  } catch (_) { $grid.hidden = true; }
}
