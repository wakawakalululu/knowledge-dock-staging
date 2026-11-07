/* 知识悬浮搜索 —— 渲染进程（通过 withGlobalTauri 注入的 window.__TAURI__ 与主进程通信） */
const { invoke } = window.__TAURI__.core;
const { listen } = window.__TAURI__.event;

const $q = document.getElementById("q");
const $results = document.getElementById("results");
const $empty = document.getElementById("empty-state");
const $settings = document.getElementById("settings");
const $cfgDir = document.getElementById("cfg-dir");
const $stats = document.getElementById("stats");
const $btnClear = document.getElementById("btn-clear");

let hits = [];          // 当前搜索结果
let active = -1;        // 键盘选中的结果下标
let debounceTimer = null;
let knowledgeDir = null;

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

/* ---------- 渲染 ---------- */
function render(query) {
  if (!hits.length) {
    $results.innerHTML =
      '<div class="empty">' +
      (query ? "没有找到相关内容" : "输入关键词，检索你的本地知识库") +
      "</div>";
    return;
  }
  $results.innerHTML = hits
    .map((h, i) => `
      <div class="hit ${i === active ? "active" : ""}" data-i="${i}">
        <div class="hit-top">
          <span class="hit-title">${highlight(h.title, query)}</span>
          <span class="hit-path">${escapeHtml(shortPath(h.path))}</span>
        </div>
        <div class="hit-snippet">${highlight(h.snippet, query)}</div>
      </div>`)
    .join("");

  $results.querySelectorAll(".hit").forEach((el) => {
    el.addEventListener("click", () => openHit(Number(el.dataset.i), false));
    el.addEventListener("dblclick", () => openHit(Number(el.dataset.i), true));
    el.addEventListener("mousemove", () => setActive(Number(el.dataset.i)));
  });
}

function setActive(i) {
  if (i === active) return;
  active = i;
  $results.querySelectorAll(".hit").forEach((el, idx) => {
    el.classList.toggle("active", idx === i);
  });
  const el = $results.querySelector(`.hit[data-i="${i}"]`);
  if (el) el.scrollIntoView({ block: "nearest" });
}

/* ---------- 动作 ---------- */
async function doSearch() {
  const query = $q.value;
  if (!query.trim()) {
    hits = [];
    active = -1;
    render("");
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

function openHit(i, reveal) {
  const h = hits[i];
  if (!h) return;
  invoke(reveal ? "reveal_file" : "open_file", { path: h.abs }).catch((e) =>
    console.error("打开失败:", e)
  );
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
    if ($q.value) {
      $q.value = "";
      $btnClear.hidden = true;
      hits = [];
      render("");
    } else {
      invoke("hide_window");
    }
  }
});

$btnClear.addEventListener("click", () => {
  $q.value = "";
  $btnClear.hidden = true;
  hits = [];
  render("");
  $q.focus();
});

document.getElementById("btn-hide").addEventListener("click", () => invoke("hide_window"));
document.getElementById("btn-quit").addEventListener("click", () => invoke("quit_app"));

document.getElementById("btn-settings").addEventListener("click", () => {
  $settings.hidden = !$settings.hidden;
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
    $settings.hidden = true;
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

/* 索引变更（文件监听触发）→ 刷新统计并重跑当前搜索 */
listen("index-updated", (ev) => {
  const s = ev.payload;
  $stats.textContent = `${s.docs} 个文档 · ${s.chunks} 个片段`;
  if ($q.value.trim()) doSearch();
});

/* ---------- 初始化 ---------- */
(async () => {
  try {
    const cfg = await invoke("get_config");
    knowledgeDir = cfg.knowledge_dir;
    if (knowledgeDir) {
      $cfgDir.textContent = knowledgeDir;
      $cfgDir.title = knowledgeDir;
    }
    const s = await invoke("rescan");
    $stats.textContent = `${s.docs} 个文档 · ${s.chunks} 个片段`;
  } catch (e) {
    if (String(e).includes("尚未设置")) {
      $stats.textContent = "请先在 ⚙ 设置中选择知识库文件夹";
      $settings.hidden = false;
    } else {
      console.error(e);
    }
  }
  $q.focus();
})();
