/* 知识悬浮搜索 —— 渲染进程（通过 withGlobalTauri 注入的 window.__TAURI__ 与主进程通信）
   形态逻辑：折叠=仅搜索条；有输入/打开设置时展开窗口并显示对应面板 */

const { invoke } = window.__TAURI__.core;
const { listen } = window.__TAURI__.event;

const $q = document.getElementById("q");
const $bar = document.getElementById("bar");
const $btnClear = document.getElementById("btn-clear");
const $results = document.getElementById("results");
const $resList = document.getElementById("res-list");
const $stats = document.getElementById("stats");
const $settings = document.getElementById("settings");
const $cfgDir = document.getElementById("cfg-dir");
const $setShortcut = document.getElementById("set-shortcut");
const $setWidth = document.getElementById("set-width");
const $setPin = document.getElementById("set-pin");

let hits = [];            // 当前搜索结果
let active = -1;          // 键盘选中的结果下标
let debounceTimer = null;
let knowledgeDir = null;
let cfg = null;           // 主进程下发的配置
let settingsOpen = false; // 设置面板展开状态
let expanded = false;     // 当前窗口形态（与后端同步）

/* ---------- 工具 ---------- */
function escapeHtml(s) {
  return s
    .replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;")
    .replace(/"/g, "&quot;").replace(/'/g, "&#39;");
}
function escapeReg(s) {
  return s.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
}

/** 把片段中的命中词包上 <mark>（大小写不敏感，先转义再匹配） */
function highlight(snippet, query) {
  const safe = escapeHtml(snippet);
  const terms = query.trim().split(/\s+/).filter(Boolean).map(escapeReg);
  if (!terms.length) return escapeHtml(snippet);
  // 按长度降序匹配，避免短词破坏长词高亮
  terms.sort((a, b) => b.length - a.length);
  const re = new RegExp("(" + terms.join("|") + ")", "gi");
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

/* ---------- 渲染 ---------- */
function render(query) {
  if (!hits.length) {
    $resList.innerHTML =
      '<div class="empty">' +
      (query ? "没有找到相关内容" : "输入关键词，检索你的本地知识库") +
      "</div>";
    return;
  }
  $resList.innerHTML = hits
    .map((h, i) => `
      <div class="hit ${i === active ? "active" : ""}" data-i="${i}">
        <div class="hit-top">
          <span class="hit-title">${highlight(h.title, query)}</span>
          <span class="hit-path">${escapeHtml(shortPath(h.path))}</span>
        </div>
        <div class="hit-snippet">${highlight(h.snippet, query)}</div>
      </div>`)
    .join("");

  $resList.querySelectorAll(".hit").forEach((el) => {
    el.addEventListener("click", () => openHit(Number(el.dataset.i), false));
    el.addEventListener("dblclick", () => openHit(Number(el.dataset.i), true));
    el.addEventListener("mousemove", () => setActive(Number(el.dataset.i)));
  });
}

function setActive(i) {
  if (i === active) return;
  active = i;
  $resList.querySelectorAll(".hit").forEach((el, idx) => {
    el.classList.toggle("active", idx === i);
  });
  const el = $resList.querySelector(`.hit[data-i="${i}"]`);
  if (el) el.scrollIntoView({ block: "nearest" });
}

/** 根据输入与面板状态刷新：面板显隐 + 窗口展开/折叠 */
function updateView() {
  const query = $q.value;
  const wantExpand = query.trim().length > 0 || settingsOpen;
  setExpanded(wantExpand);
  $results.hidden = query.trim().length === 0;
  $settings.hidden = !settingsOpen;
  render(query);
}

/* ---------- 动作 ---------- */
async function doSearch() {
  const query = $q.value;
  if (!query.trim()) {
    hits = [];
    active = -1;
    updateView();
    return;
  }
  try {
    hits = await invoke("search", { q: query });
  } catch (e) {
    hits = [];
    console.error(e);
  }
  active = hits.length ? 0 : -1;
  render(query);
}

function clearInput() {
  $q.value = "";
  $btnClear.hidden = true;
  hits = [];
  active = -1;
  updateView();
}

function openHit(i, reveal) {
  const h = hits[i];
  if (!h) return;
  invoke(reveal ? "reveal_file" : "open_file", { path: h.abs }).catch((e) =>
    console.error("打开失败:", e)
  );
}

/* ---------- 设置面板 ---------- */
function openSettings() {
  settingsOpen = true;
  if (cfg) {
    $setShortcut.value = cfg.shortcut;
    $setWidth.value = cfg.width;
    $setPin.checked = cfg.pin_mode;
  }
  updateView();
}
function closeSettings() {
  settingsOpen = false;
  updateView();
}

/* ---------- 事件绑定 ---------- */
$q.addEventListener("input", () => {
  $btnClear.hidden = !$q.value;
  clearTimeout(debounceTimer);
  debounceTimer = setTimeout(doSearch, 150);
});

$q.addEventListener("keydown", (e) => {
  if (e.key === "ArrowDown") {
    e.preventDefault();
    if (hits.length) setActive(Math.min(active + 1, hits.length - 1));
  } else if (e.key === "ArrowUp") {
    e.preventDefault();
    if (hits.length) setActive(Math.max(active - 1, 0));
  } else if (e.key === "Enter") {
    if (hits.length) openHit(active < 0 ? 0 : active, e.ctrlKey);
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

document.getElementById("btn-pick").addEventListener("click", async () => {
  $stats.textContent = "扫描中…";
  const r = await invoke("pick_folder").catch((e) => ({ error: String(e) }));
  if (r.error) {
    $stats.textContent = "扫描失败: " + r.error;
  } else if (r.dir) {
    knowledgeDir = r.dir;
    $cfgDir.textContent = r.dir;
    $cfgDir.title = r.dir;
    $stats.textContent = `${r.stats.docs} 个文档 · ${r.stats.chunks} 个片段`;
    $q.focus();
  } else {
    $stats.textContent = "就绪";
  }
});

document.getElementById("btn-rescan").addEventListener("click", async () => {
  if (!knowledgeDir) return;
  $stats.textContent = "扫描中…";
  try {
    const s = await invoke("rescan");
    $stats.textContent = `${s.docs} 个文档 · ${s.chunks} 个片段`;
  } catch (e) {
    $stats.textContent = "扫描失败: " + e;
  }
});

document.getElementById("btn-apply").addEventListener("click", async () => {
  if (!cfg) return;
  const next = {
    ...cfg,
    shortcut: $setShortcut.value.trim() || cfg.shortcut,
    width: Number($setWidth.value) || cfg.width,
    pin_mode: $setPin.checked,
  };
  try {
    cfg = await invoke("save_config", { cfg: next });
    applyPin();
    // 宽度立即生效（以当前形态重新应用一次尺寸）
    expanded = !expanded; // 强制 setExpanded 触发后端 resize
    setExpanded(!expanded);
    closeSettings();
    $q.focus();
  } catch (e) {
    $stats.textContent = "保存失败: " + e;
  }
});

document.getElementById("btn-quit").addEventListener("click", () => invoke("quit_app"));

/* 索引变更（文件监听触发）→ 刷新统计并重跑当前搜索 */
listen("index-updated", (ev) => {
  const s = ev.payload;
  $stats.textContent = `${s.docs} 个文档 · ${s.chunks} 个片段`;
  if ($q.value.trim()) doSearch();
});

/* 后端唤起（快捷键/二次启动/托盘）→ 复位为折叠形态 */
listen("ui-reset", () => {
  $q.value = "";
  $btnClear.hidden = true;
  hits = [];
  active = -1;
  settingsOpen = false;
  updateView();
  $q.focus();
});

/* ---------- 初始化 ---------- */
(async () => {
  try {
    cfg = await invoke("get_config");
    knowledgeDir = cfg.knowledge_dir;
    if (knowledgeDir) {
      $cfgDir.textContent = knowledgeDir;
      $cfgDir.title = knowledgeDir;
    }
    $setShortcut.value = cfg.shortcut;
    $setWidth.value = cfg.width;
    $setPin.checked = cfg.pin_mode;
    applyPin();
    const s = await invoke("rescan");
    $stats.textContent = `${s.docs} 个文档 · ${s.chunks} 个片段`;
  } catch (e) {
    if (String(e).includes("尚未设置")) {
      $stats.textContent = "请先在设置中选择知识库文件夹";
      openSettings();
    } else {
      console.error(e);
    }
  }
  $q.focus();
})();
