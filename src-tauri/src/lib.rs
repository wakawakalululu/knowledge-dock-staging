//! 知识悬浮搜索 —— uTools 风格的本地知识库常驻置顶检索窗（逻辑主体）
//!
//! 形态：折叠时仅一条搜索框高度，检索时向下展开结果面板；
//! 全局快捷键唤起/隐藏，失焦自动隐藏（钉住模式除外）。
//! 检索：Tantivy BM25 + jieba 分词 + 持久化索引 + 同义词改写（纯离线）。

pub mod config;
pub mod matching;
pub mod scope;
pub mod engine;
pub mod gitsync;
pub mod indexer;
pub mod webdav;

#[cfg(test)]
mod search_tests;

use config::Config;
use engine::{Engine, SearchResponse, StatsDetail};
use indexer::Stats;
use notify::{RecommendedWatcher, RecursiveMode, Watcher};
use serde::Serialize;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};
use tauri::menu::{Menu, MenuItem};
use tauri::tray::TrayIconBuilder;
use tauri::{AppHandle, Emitter, Manager, State, WindowEvent};
use tauri_plugin_global_shortcut::{GlobalShortcutExt, ShortcutState};

/// 折叠（仅搜索框）/ 展开（含结果面板）的窗口逻辑高度
const COLLAPSED_H: f64 = 76.0;
const EXPANDED_H: f64 = 600.0;

struct AppState {
    engine: Arc<Engine>,
    cfg: Mutex<Config>,
    /// 原生目录选择对话框打开期间禁止失焦隐藏
    dialog_open: AtomicBool,
    /// 文件监听句柄：替换即关闭旧通道，旧监听线程随之退出
    watcher: Mutex<Option<RecommendedWatcher>>,
    /// git 操作互斥（克隆/拉取串行，防定时与手动并发冲突）
    git_lock: Mutex<()>,
    /// WebDAV 同步互斥
    webdav_lock: Mutex<()>,
}

/// 拖动节流：窗口移动事件的保存间隔
static LAST_SAVE_MS: AtomicU64 = AtomicU64::new(0);
/// 最新窗口位置（物理像素），供退出时兜底保存
static LAST_X: AtomicI64 = AtomicI64::new(i64::MIN);
static LAST_Y: AtomicI64 = AtomicI64::new(i64::MIN);
/// 前端就绪信号：窗口默认隐藏，前端加载完成后调用 frontend_ready 再显示；
/// 防止 webview 未渲染完成的黑屏/空窗闪烁
static FRONTEND_SHOWN: AtomicBool = AtomicBool::new(false);

#[derive(Serialize)]
struct PickResult {
    dir: Option<String>,
    /// 追加后的完整目录列表
    dirs: Vec<String>,
    stats: Option<Stats>,
    error: Option<String>,
}

#[derive(Serialize)]
struct ContextResult {
    content: String,
    start_line: usize,
    end_line: usize,
    match_line: usize,
}

/// 后台执行一次多根同步并推送统计
fn sync_and_emit(app: &AppHandle) {
    let state: State<AppState> = app.state();
    let roots = state.cfg.lock().expect("cfg lock").root_specs();
    let scope = state.cfg.lock().expect("cfg lock").scope.clone();
    if roots.is_empty() {
        return;
    }
    match state.engine.sync_roots(&roots, &scope) {
        Ok(stats) => {
            let _ = app.emit("index-updated", &stats);
        }
        Err(e) => {
            let _ = app.emit("index-error", e);
        }
    }
}

/// 启动/替换文件监听：一个 watcher 监听全部知识库根；
/// 旧 watcher 被替换后通道关闭，旧监听线程自动退出（防线程累积）
fn start_watchers(app: &AppHandle) {
    let state: State<AppState> = app.state();
    let roots = state.cfg.lock().expect("cfg lock").root_specs();
    if roots.is_empty() {
        return;
    }
    let (tx, rx) = std::sync::mpsc::channel();
    let mut watcher = match notify::recommended_watcher(tx) {
        Ok(w) => w,
        Err(_) => return,
    };
    for spec in &roots {
        if watcher.watch(&spec.path, RecursiveMode::Recursive).is_err() {
            return;
        }
    }

    // 泵线程：静默 900ms 后触发多根增量同步
    let handle = app.clone();
    std::thread::spawn(move || {
        loop {
            // 通道关闭（watcher 被替换）则退出线程
            if rx.recv().is_err() {
                return;
            }
            loop {
                match rx.recv_timeout(Duration::from_millis(900)) {
                    Ok(_) => continue,
                    Err(_) => break,
                }
            }
            sync_and_emit(&handle);
        }
    });

    *state.watcher.lock().expect("watcher lock") = Some(watcher);
}

/// 以折叠形态显示并聚焦（打开即一条搜索框，uTools 式）
fn reveal(app: &AppHandle) {
    let state: State<AppState> = app.state();
    let width = state.cfg.lock().expect("cfg lock").width;
    if let Some(win) = app.get_webview_window("main") {
        let _ = win.set_size(tauri::LogicalSize::new(width, COLLAPSED_H));
        let _ = win.show();
        let _ = win.set_focus();
        // 通知前端复位：清空输入、收起面板
        let _ = app.emit("ui-reset", ());
    }
}

fn toggle_visible(app: &AppHandle) {
    if let Some(win) = app.get_webview_window("main") {
        if win.is_visible().unwrap_or(false) {
            let _ = win.hide();
        } else {
            reveal(app);
        }
    }
}

/// 注册（或换绑）全局快捷键；失败返回错误由调用方回退默认值
fn apply_shortcut(app: &AppHandle, accel: &str) -> Result<(), String> {
    let accel = accel.trim();
    let accel = if accel.is_empty() { config::DEFAULT_SHORTCUT } else { accel };
    let gs = app.global_shortcut();
    let _ = gs.unregister_all();
    gs.on_shortcut(accel.to_lowercase().as_str(), |app, _sc, event| {
        if event.state == ShortcutState::Pressed {
            toggle_visible(app);
        }
    })
    .map_err(|e| e.to_string())
}

/// 展开后若超出屏幕底部则上移，保证面板完整可见
fn clamp_into_monitor(app: &AppHandle) {
    let (Some(win), Ok(monitors)) = (
        app.get_webview_window("main"),
        app.available_monitors(),
    ) else { return };
    let (Ok(pos), Ok(size)) = (win.outer_position(), win.outer_size()) else { return };
    let h = size.height as i32;
    for m in monitors {
        let (mx, my) = (m.position().x, m.position().y);
        let (mw, mh) = (m.size().width as i32, m.size().height as i32);
        if pos.x >= mx && pos.x < mx + mw && pos.y >= my && pos.y < my + mh {
            let mut ny = pos.y;
            if pos.y + h > my + mh {
                ny = my + mh - h;
            }
            if ny < my {
                ny = my;
            }
            if ny != pos.y {
                let _ = win.set_position(tauri::PhysicalPosition::new(pos.x, ny));
            }
            break;
        }
    }
}

// ---------------- Tauri 命令 ----------------

#[tauri::command]
fn get_config(state: State<'_, AppState>) -> Config {
    state.cfg.lock().expect("cfg lock").clone()
}

#[tauri::command]
fn save_config(app: AppHandle, cfg: Config, state: State<'_, AppState>) -> Result<Config, String> {
    let mut cfg = cfg;
    if !(320.0..=1600.0).contains(&cfg.width) {
        cfg.width = config::DEFAULT_WIDTH;
    }
    let accel = cfg.shortcut.trim().to_string();
    cfg.shortcut = if accel.is_empty() {
        config::DEFAULT_SHORTCUT.to_string()
    } else {
        accel
    };
    // 换绑快捷键；失败则保持旧键并报错
    apply_shortcut(&app, &cfg.shortcut).map_err(|e| format!("快捷键注册失败: {e}"))?;
    *state.cfg.lock().expect("cfg lock") = cfg.clone();
    config::save(&app, &cfg);
    // 自动备份轮换
    let bdir = app.path().app_config_dir().unwrap_or_default().join("config-backups");
    let _ = std::fs::create_dir_all(&bdir);
    if let Ok(cur) = std::fs::read_to_string(app.path().app_config_dir().unwrap_or_default().join("config.json")) {
        let ts = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
        let _ = std::fs::write(bdir.join(format!("config-{ts}.json")), cur);
        let mut bs: Vec<String> = std::fs::read_dir(&bdir).map(|entries| entries.filter_map(|e| e.ok()).filter_map(|e| e.file_name().to_str().map(String::from)).collect()).unwrap_or_default();
        bs.sort();
        while bs.len() > 5 { let _ = std::fs::remove_file(bdir.join(&bs[0])); bs.remove(0); }
    }
    Ok(cfg)
}

/// 前端驱动窗口折叠/展开（WebView2 命中测试是窗口级的，
/// 采用动态调高代替固定大窗+局部穿透）
#[tauri::command]
fn ui_mode(app: AppHandle, mode: String, state: State<'_, AppState>) {
    let width = state.cfg.lock().expect("cfg lock").width;
    let height = if mode == "expanded" { EXPANDED_H } else { COLLAPSED_H };
    if let Some(win) = app.get_webview_window("main") {
        let _ = win.set_size(tauri::LogicalSize::new(width, height));
        clamp_into_monitor(&app);
    }
}

#[tauri::command]
fn search(
    q: String,
    type_filter: Option<String>,
    root_filter: Option<String>,
    limit: Option<usize>,
    state: State<'_, AppState>,
) -> SearchResponse {
    let start = std::time::Instant::now();
    let mut resp = state.engine.search(
        &q,
        limit.unwrap_or_else(searchLimit),
        type_filter.as_deref(),
        root_filter.as_deref(),
    );
    resp.duration_ms = start.elapsed().as_millis() as u64;
    resp
}

fn searchLimit() -> usize { 50 }

/// 添加搜索记录到历史（去重、最新在前、最多 20 条），返回更新后的历史
#[tauri::command]
fn add_search_history(q: String, app: AppHandle, state: State<'_, AppState>) -> Vec<String> {
    let q = q.trim().to_string();
    let mut cfg = state.cfg.lock().expect("cfg lock");
    if !q.is_empty() {
        cfg.search_history.retain(|h| h != &q);
        cfg.search_history.insert(0, q);
        cfg.search_history.truncate(20);
    }
    let history = cfg.search_history.clone();
    config::save(&app, &cfg);
    history
}

/// 获取搜索历史
#[tauri::command]
fn get_search_history(state: State<'_, AppState>) -> Vec<String> {
    state.cfg.lock().expect("cfg lock").search_history.clone()
}

/// 清空搜索历史
#[tauri::command]
fn clear_search_history(app: AppHandle, state: State<'_, AppState>) {
    state.cfg.lock().expect("cfg lock").search_history.clear();
    let cfg = state.cfg.lock().expect("cfg lock").clone();
    config::save(&app, &cfg);
}

/// 获取范围规则模板（内嵌 JSON）
#[tauri::command]
fn get_scope_templates() -> String {
    include_str!("../assets/scope-templates.json").to_string()
}

/// 应用范围规则模板
#[tauri::command]
fn apply_scope_template(name: String, app: AppHandle, state: State<'_, AppState>) -> Result<Config, String> {
    let templates: Vec<serde_json::Value> = serde_json::from_str(include_str!("../assets/scope-templates.json")).map_err(|e| e.to_string())?;
    let tpl = templates.iter().find(|t| t["name"] == name).ok_or_else(|| format!("模板不存在: {name}"))?;
    let mut cfg = state.cfg.lock().expect("cfg lock").clone();
    if let Some(ex) = tpl["exclude_globs"].as_array() { cfg.scope.exclude_globs = ex.iter().filter_map(|v| v.as_str().map(String::from)).collect(); }
    if let Some(exts) = tpl["include_exts"].as_array() { cfg.scope.include_exts = exts.iter().filter_map(|v| v.as_str().map(String::from)).collect(); }
    config::save(&app, &cfg);
    let roots = cfg.root_specs();
    let scope = cfg.scope.clone();
    let _ = state.engine.sync_roots(&roots, &scope);
    Ok(cfg)
}

/// 保存配置文件
#[tauri::command]
fn save_config_profile(app: AppHandle, name: String, state: State<'_, AppState>) -> Result<(), String> {
    let cfg = state.cfg.lock().expect("cfg lock").clone();
    let dir = app.path().app_config_dir().map_err(|e| e.to_string())?.join("profiles");
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let json = serde_json::to_string_pretty(&cfg).map_err(|e| e.to_string())?;
    std::fs::write(dir.join(format!("{name}.json")), json).map_err(|e| e.to_string())
}

/// 列出配置文件
#[tauri::command]
fn list_config_profiles(app: AppHandle) -> Vec<String> {
    let dir = app.path().app_config_dir().unwrap_or_default().join("profiles");
    std::fs::read_dir(&dir).map(|entries| entries.filter_map(|e| e.ok()).filter(|e| e.file_name().to_str().map(|s| s.ends_with(".json")).unwrap_or(false)).filter_map(|e| e.file_name().to_str().map(|s| s.trim_end_matches(".json").to_string())).collect()).unwrap_or_default()
}

/// 加载配置文件
#[tauri::command]
fn load_config_profile(app: AppHandle, name: String, state: State<'_, AppState>) -> Result<Config, String> {
    let dir = app.path().app_config_dir().map_err(|e| e.to_string())?;
    let path = dir.join("profiles").join(format!("{name}.json"));
    let json = std::fs::read_to_string(&path).map_err(|e| e.to_string())?;
    let cfg: Config = serde_json::from_str(&json).map_err(|e| format!("配置格式错误: {e}"))?;
    apply_shortcut(&app, &cfg.shortcut).map_err(|e| format!("快捷键注册失败: {e}"))?;
    *state.cfg.lock().expect("cfg lock") = cfg.clone();
    config::save(&app, &cfg);
    let roots = cfg.root_specs();
    if !roots.is_empty() { let scope = cfg.scope.clone(); let _ = state.engine.sync_roots(&roots, &scope); start_watchers(&app); }
    Ok(cfg)
}

/// 导出范围规则 JSON
#[tauri::command]
fn export_scope_rules(state: State<'_, AppState>) -> Result<String, String> {
    let cfg = state.cfg.lock().expect("cfg lock");
    let data = serde_json::json!({
        "exclude_globs": cfg.scope.exclude_globs,
        "include_globs": cfg.scope.include_globs,
        "include_exts": cfg.scope.include_exts,
        "max_file_kb": cfg.scope.max_file_kb,
        "max_age_days": cfg.scope.max_age_days
    });
    Ok(serde_json::to_string_pretty(&data).unwrap_or_default())
}

/// 导入范围规则 JSON
#[tauri::command]
fn import_scope_rules(app: AppHandle, json: String, state: State<'_, AppState>) -> Result<Config, String> {
    let data: serde_json::Value = serde_json::from_str(&json).map_err(|e| format!("JSON 格式错误: {e}"))?;
    let mut cfg = state.cfg.lock().expect("cfg lock").clone();
    if let Some(ex) = data["exclude_globs"].as_array() { cfg.scope.exclude_globs = ex.iter().filter_map(|v| v.as_str().map(String::from)).collect(); }
    if let Some(inc) = data["include_globs"].as_array() { cfg.scope.include_globs = inc.iter().filter_map(|v| v.as_str().map(String::from)).collect(); }
    if let Some(exts) = data["include_exts"].as_array() { cfg.scope.include_exts = exts.iter().filter_map(|v| v.as_str().map(String::from)).collect(); }
    if let Some(kb) = data["max_file_kb"].as_u64() { cfg.scope.max_file_kb = kb; }
    if let Some(days) = data["max_age_days"].as_u64() { cfg.scope.max_age_days = days; }
    config::save(&app, &cfg);
    let roots = cfg.root_specs();
    let scope = cfg.scope.clone();
    let _ = state.engine.sync_roots(&roots, &scope);
    Ok(cfg)
}

/// 索引统计仪表盘：各类型/各库块数分布
#[tauri::command]
fn get_index_stats(state: State<'_, AppState>) -> StatsDetail {
    state.engine.stats_detail()
}

/// 范围诊断：判定某文件当前配置下是否会被索引，逐条给出判定链
#[derive(Serialize)]
struct ScopeCheckResult {
    exists: bool,
    /// 归属的知识库根（显示名）；不在任何启用根内则 None
    root: Option<String>,
    /// 相对根的路径
    rel: Option<String>,
    will_index: bool,
    /// 判定链（按扫描器实际顺序）
    reasons: Vec<String>,
}

#[tauri::command]
fn scope_check(path: String, state: State<'_, AppState>) -> ScopeCheckResult {
    use crate::matching::{glob_rule_matches, is_included_rel};
    let cfg = state.cfg.lock().expect("cfg lock").clone();
    let scope = &cfg.scope;
    let mut reasons: Vec<String> = Vec::new();
    let fail = |root: Option<String>, rel: Option<String>, mut reasons: Vec<String>| ScopeCheckResult {
        exists: true,
        root,
        rel,
        will_index: false,
        reasons: {
            reasons.push("结论: 不会被索引".into());
            reasons
        },
    };

    let p = std::path::PathBuf::from(path.trim());
    if !p.is_file() {
        return ScopeCheckResult {
            exists: false,
            root: None,
            rel: None,
            will_index: false,
            reasons: vec!["路径不存在或不是文件".into()],
        };
    }
    let pstr = p.to_string_lossy().to_string();
    // 归一化正反斜杠后比较（用户输入 C:/ 而根配置多为 C:\）
    let pnorm = pstr.replace('\\', "/");
    let pl = pnorm.to_lowercase();

    // 归属根：取前缀最长（最具体）的启用根
    let mut best: Option<(usize, config::KnowledgeRoot, String)> = None;
    for r in cfg.effective_roots() {
        let rp = r
            .path
            .replace('\\', "/")
            .trim_end_matches('/')
            .to_string();
        if rp.len() <= 2 || !pl.starts_with(&rp.to_lowercase()) {
            continue;
        }
        let rel = pnorm[rp.len()..].trim_start_matches('/').to_string();
        if best.as_ref().map_or(true, |(len, _, _)| rp.len() > *len) {
            best = Some((rp.len(), r, rel));
        }
    }
    let Some((_len, root, rel)) = best else {
        return ScopeCheckResult {
            exists: true,
            root: None,
            rel: None,
            will_index: false,
            reasons: vec!["不在任何启用的知识库根内", "结论: 不会被索引"]
                .into_iter()
                .map(String::from)
                .collect(),
        };
    };
    let label = if root.label.trim().is_empty() {
        scope::basename_of(&root.path)
    } else {
        root.label.clone()
    };
    reasons.push(format!("归属库: {label}（相对路径 {rel}）"));

    // 忽略目录（扫描层同款规则：. 开头或内置忽略名单）
    if indexer::is_ignored_rel(&rel) {
        reasons.push("忽略目录 ✗ 路径含 . 开头或内置忽略目录（node_modules/target 等）".into());
        return fail(Some(label), Some(rel), reasons);
    }
    reasons.push("忽略目录 ✓".into());

    // 扩展名（每库覆盖优先于全局）
    let ext = p.extension().map(|e| e.to_string_lossy().to_lowercase());
    let (exts, ext_src) = if !root.root_exts.is_empty() {
        (root.root_exts.clone(), "每库覆盖")
    } else if scope.include_exts.is_empty() {
        (vec!["md".into(), "markdown".into(), "txt".into()], "默认")
    } else {
        (scope.include_exts.clone(), "全局")
    };
    let ext_ok = matches!(ext.as_deref(), Some(x) if exts.iter().any(|e| e == x));
    if ext_ok {
        reasons.push(format!("扩展名 ✓ ({:?} [{}])", ext.unwrap_or_default(), ext_src));
    } else {
        reasons.push(format!(
            "扩展名 ✗ {:?} 不在 [{}]（{}）",
            ext,
            exts.join(", "),
            ext_src
        ));
        return fail(Some(label), Some(rel), reasons);
    }

    // 大小上限（每库覆盖优先于全局）
    let max_kb = if root.root_max_file_kb > 0 {
        root.root_max_file_kb
    } else if scope.max_file_kb == 0 {
        4096
    } else {
        scope.max_file_kb
    };
    let kb_src = if root.root_max_file_kb > 0 { "每库覆盖" } else { "全局" };
    let max_bytes = max_kb * 1024;
    let (size_ok, size_kb) = std::fs::metadata(&p)
        .map(|m| (m.len() <= max_bytes, m.len() / 1024))
        .unwrap_or((false, 0));
    if size_ok {
        reasons.push(format!("大小 ✓ ({size_kb} KB ≤ {max_kb} KB [{kb_src}])"));
    } else {
        reasons.push(format!("大小 ✗ {size_kb} KB 超过上限 {max_kb} KB（{kb_src}）"));
        return fail(Some(label), Some(rel), reasons);
    }

    // 白名单（空 = 全收）
    if scope.include_globs.is_empty() {
        reasons.push("包含白名单 ✓ 未启用（全收）".into());
    } else if is_included_rel(&rel, &scope.include_globs) {
        let hit = scope
            .include_globs
            .iter()
            .find(|r| glob_rule_matches(&rel, r))
            .cloned()
            .unwrap_or_default();
        reasons.push(format!("包含白名单 ✓ 命中: {hit}"));
    } else {
        reasons.push(format!(
            "包含白名单 ✗ 未命中任何规则 [{}]",
            scope.include_globs.join(", ")
        ));
        return fail(Some(label), Some(rel), reasons);
    }

    // 排除规则（全局 + 每库独立）
    let all_ex: Vec<String> = scope
        .exclude_globs
        .iter()
        .chain(root.root_exclude_globs.iter())
        .cloned()
        .collect();
    if let Some(hit) = all_ex.iter().find(|r| glob_rule_matches(&rel, r)) {
        reasons.push(format!("排除规则 ✗ 命中: {hit}"));
        return fail(Some(label), Some(rel), reasons);
    }
    reasons.push("排除规则 ✓ 未命中".into());

    // 时间窗
    if scope.max_age_days == 0 {
        reasons.push("时间窗 ✓ 未启用".into());
    } else {
        let mtime = indexer::mtime_ms(&p);
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);
        let cutoff = now_ms.saturating_sub(scope.max_age_days.saturating_mul(86_400_000));
        if mtime >= cutoff {
            reasons.push(format!("时间窗 ✓ 最近 {} 天内有修改", scope.max_age_days));
        } else {
            reasons.push(format!("时间窗 ✗ 超过 {} 天未修改", scope.max_age_days));
            return fail(Some(label), Some(rel), reasons);
        }
    }

    reasons.push("结论: 会被索引".into());
    ScopeCheckResult {
        exists: true,
        root: Some(label),
        rel: Some(rel),
        will_index: true,
        reasons,
    }
}

// ---------------- 配置健康检查 ----------------

/// 健康问题条目
#[derive(Serialize)]
struct HealthIssue {
    /// duplicate_root | missing_path | bad_glob | git_source_broken | git_root_missing | bad_value
    kind: String,
    /// error | warn
    severity: String,
    message: String,
    fixable: bool,
}

/// 规则归一化：反斜杠→正斜杠、去空白、去盘符/前导斜杠（相对规则误填绝对路径时）
fn normalize_rule(r: &str) -> String {
    let mut s = r.trim().replace('\\', "/").to_string();
    // 盘符前缀（C:/...）→ 相对化
    if s.len() >= 2 && s.as_bytes()[1] == b':' {
        s = s[2..].to_string();
    }
    s.trim_start_matches('/').to_string()
}

/// 收集当前配置的健康问题（与 fix_all 的修复逻辑一一对应）
fn collect_health_issues(cfg: &Config) -> Vec<HealthIssue> {
    let mut issues: Vec<HealthIssue> = Vec::new();

    // 1. 重复知识库根（归一化后 path 相同）
    let mut seen: HashMap<String, usize> = HashMap::new();
    for (i, r) in cfg.roots.iter().enumerate() {
        let key = r.path.replace('\\', "/").trim_end_matches('/').to_lowercase();
        if let Some(&first) = seen.get(&key) {
            issues.push(HealthIssue {
                kind: "duplicate_root".into(),
                severity: "error".into(),
                message: format!(
                    "重复知识库根: {}（与第 {} 条相同）",
                    r.path,
                    first + 1
                ),
                fixable: true,
            });
        } else {
            seen.insert(key, i);
        }
    }

    // 2. 根目录不存在
    for r in &cfg.roots {
        if !std::path::Path::new(&r.path).is_dir() {
            issues.push(HealthIssue {
                kind: "missing_path".into(),
                severity: "error".into(),
                message: format!("目录不存在: {}（{}）", r.path, r.label),
                fixable: true,
            });
        }
    }

    // 3. 无效/不规范 glob 规则（全局 + 每库独立）
    let check_rules = |name: &str, rules: &[String], out: &mut Vec<HealthIssue>| {
        let mut norm_seen: HashMap<String, usize> = HashMap::new();
        for (i, raw) in rules.iter().enumerate() {
            if raw.trim().is_empty() {
                out.push(HealthIssue {
                    kind: "bad_glob".into(),
                    severity: "warn".into(),
                    message: format!("{name} 第 {} 条为空规则", i + 1),
                    fixable: true,
                });
                continue;
            }
            let n = normalize_rule(raw);
            if *raw != n {
                out.push(HealthIssue {
                    kind: "bad_glob".into(),
                    severity: "warn".into(),
                    message: format!("{name} 第 {} 条「{raw}」含反斜杠/盘符/前导斜杠，将归一为「{n}」", i + 1),
                    fixable: true,
                });
            }
            let cnt = norm_seen.entry(n).or_insert(0);
            *cnt += 1;
            if *cnt == 2 {
                out.push(HealthIssue {
                    kind: "bad_glob".into(),
                    severity: "warn".into(),
                    message: format!("{name} 存在重复规则「{}」", raw.trim()),
                    fixable: true,
                });
            }
        }
    };
    check_rules("排除规则", &cfg.scope.exclude_globs, &mut issues);
    check_rules("包含白名单", &cfg.scope.include_globs, &mut issues);
    for r in &cfg.roots {
        check_rules(&format!("库「{}」独立排除", r.label), &r.root_exclude_globs, &mut issues);
    }

    // 4. git 源健康检查需要真实 repos 路径，在 health_check 命令层执行

    // 5. 值域异常
    if cfg.scope.max_file_kb == 0 {
        issues.push(HealthIssue {
            kind: "bad_value".into(),
            severity: "warn".into(),
            message: "单文件大小上限为 0（等于禁用所有文件）".into(),
            fixable: true,
        });
    }
    if cfg.scope.max_age_days > 36500 {
        issues.push(HealthIssue {
            kind: "bad_value".into(),
            severity: "warn".into(),
            message: format!("时间窗 {} 天异常（超过 100 年）", cfg.scope.max_age_days),
            fixable: true,
        });
    }
    let bad_exts: Vec<String> = cfg
        .scope
        .include_exts
        .iter()
        .filter(|e| e.trim().is_empty() || e.starts_with('.') || **e != e.to_lowercase())
        .cloned()
        .collect();
    if !bad_exts.is_empty() {
        issues.push(HealthIssue {
            kind: "bad_value".into(),
            severity: "warn".into(),
            message: format!("扩展名列表不规范: [{}]（含空项/点前缀/大写）", bad_exts.join(", ")),
            fixable: true,
        });
    }

    // 6. 每库覆盖值域
    for r in &cfg.roots {
        let bad: Vec<String> = r
            .root_exts
            .iter()
            .filter(|e| e.trim().is_empty() || e.starts_with('.') || **e != e.to_lowercase())
            .cloned()
            .collect();
        if !bad.is_empty() {
            issues.push(HealthIssue {
                kind: "bad_value".into(),
                severity: "warn".into(),
                message: format!(
                    "库「{}」扩展名覆盖不规范: [{}]（含空项/点前缀/大写）",
                    r.label,
                    bad.join(", ")
                ),
                fixable: true,
            });
        }
        if r.root_max_file_kb > 1_000_000_000 {
            issues.push(HealthIssue {
                kind: "bad_value".into(),
                severity: "warn".into(),
                message: format!(
                    "库「{}」大小上限覆盖异常: {} KB（超过 1TB）",
                    r.label, r.root_max_file_kb
                ),
                fixable: true,
            });
        }
    }

    // 7. WebDAV 源健康：镜像目录缺失（真实检查在命令层带路径重做）

    issues
}

/// 配置健康检查：返回问题清单（fixable 项可被 health_fix_all 修复）
#[tauri::command]
fn health_check(app: AppHandle, state: State<'_, AppState>) -> Vec<HealthIssue> {
    let cfg = state.cfg.lock().expect("cfg lock").clone();
    let mut issues = collect_health_issues(&cfg);
    // git 源检查需要真实 repos 路径，在此补齐
    let repos = repos_dir(&app);
    for s in &cfg.git_sources {
        let dir = repos.join(&s.name);
        if !dir.join(".git").exists() {
            issues.push(HealthIssue {
                kind: "git_source_broken".into(),
                severity: "error".into(),
                message: format!("git 源「{}」的本地克隆缺失（{}）", s.name, dir.display()),
                fixable: true,
            });
        }
        let dir_str = dir.to_string_lossy().to_string();
        if !cfg.roots.iter().any(|r| r.path == dir_str) {
            issues.push(HealthIssue {
                kind: "git_root_missing".into(),
                severity: "warn".into(),
                message: format!("git 源「{}」未注册为知识根", s.name),
                fixable: true,
            });
        }
    }
    // WebDAV 源检查：镜像目录缺失
    let dav_root = webdav_dir(&app);
    for s in &cfg.webdav_sources {
        let dir = dav_root.join(&s.name);
        if !dir.is_dir() {
            issues.push(HealthIssue {
                kind: "webdav_source_broken".into(),
                severity: "error".into(),
                message: format!("WebDAV 源「{}」的本地镜像缺失（{}）", s.name, dir.display()),
                fixable: true,
            });
        }
        let dir_str = dir.to_string_lossy().to_string();
        if !cfg.roots.iter().any(|r| r.path == dir_str) {
            issues.push(HealthIssue {
                kind: "webdav_root_missing".into(),
                severity: "warn".into(),
                message: format!("WebDAV 源「{}」未注册为知识根", s.name),
                fixable: true,
            });
        }
    }
    issues
}

/// 一键修复全部可修复项：去重根、移除失效根、清洗规则、补齐 git 根、回正值域
#[tauri::command]
fn health_fix_all(app: AppHandle, state: State<'_, AppState>) -> Result<(Vec<String>, Config), String> {
    let mut cfg = state.cfg.lock().expect("cfg lock").clone();
    let mut fixed: Vec<String> = Vec::new();

    // 1. 去重根（保留首条）
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    let before = cfg.roots.len();
    cfg.roots.retain(|r| {
        seen.insert(r.path.replace('\\', "/").trim_end_matches('/').to_lowercase())
    });
    if cfg.roots.len() < before {
        fixed.push(format!("移除重复知识库根 {} 条", before - cfg.roots.len()));
    }

    // 2. 移除目录不存在的根
    let before = cfg.roots.len();
    cfg.roots.retain(|r| std::path::Path::new(&r.path).is_dir());
    if cfg.roots.len() < before {
        fixed.push(format!("移除目录不存在的知识库根 {} 条", before - cfg.roots.len()));
    }

    // 3. 清洗 glob 规则（去空、归一、去重）
    let clean = |rules: &Vec<String>| -> (Vec<String>, bool) {
        let mut out: Vec<String> = Vec::new();
        let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
        let mut changed = false;
        for raw in rules {
            let n = normalize_rule(raw);
            if n.is_empty() {
                changed = true;
                continue;
            }
            if &n != raw {
                changed = true;
            }
            if seen.insert(n.clone()) {
                out.push(n);
            } else {
                changed = true;
            }
        }
        (out, changed)
    };
    let (ex, ch) = clean(&cfg.scope.exclude_globs);
    if ch { fixed.push("清洗全局排除规则".into()); }
    cfg.scope.exclude_globs = ex;
    let (inc, ch) = clean(&cfg.scope.include_globs);
    if ch { fixed.push("清洗包含白名单规则".into()); }
    cfg.scope.include_globs = inc;
    for r in &mut cfg.roots {
        let (re, ch) = clean(&r.root_exclude_globs);
        if ch { fixed.push(format!("清洗库「{}」独立排除规则", r.label)); }
        r.root_exclude_globs = re;
        // 每库扩展名覆盖清洗
        let before = r.root_exts.clone();
        r.root_exts = r
            .root_exts
            .iter()
            .map(|e| e.trim().trim_start_matches('.').to_lowercase())
            .filter(|e| !e.is_empty())
            .collect();
        if r.root_exts != before {
            fixed.push(format!("清洗库「{}」扩展名覆盖", r.label));
        }
        // 每库大小上限异常回正（0 = 用全局）
        if r.root_max_file_kb > 1_000_000_000 {
            r.root_max_file_kb = 0;
            fixed.push(format!("库「{}」大小上限覆盖异常回正为用全局", r.label));
        }
    }

    // 4. git 源：克隆缺失→移除源（根若存在一并移除）；根缺失→补注册
    let repos = repos_dir(&app);
    let before = cfg.git_sources.len();
    cfg.git_sources.retain(|s| repos.join(&s.name).join(".git").exists());
    if cfg.git_sources.len() < before {
        fixed.push(format!("移除克隆缺失的 git 源 {} 条", before - cfg.git_sources.len()));
    }
    let before = cfg.roots.len();
    cfg.roots.retain(|r| {
        // 孤儿根：指向 repos 目录但没有对应 git 源 → 移除
        let p = r.path.replace('\\', "/");
        if let Some(idx) = p.rfind("/repos/") {
            let name = p[idx + 7..].trim_end_matches('/').to_string();
            if cfg.git_sources.iter().any(|s| s.name == name) {
                true
            } else {
                fixed.push(format!("移除孤儿克隆根「{}」", r.label));
                false
            }
        } else {
            true
        }
    });
    let _ = before;
    for s in &cfg.git_sources {
        let dir_str = repos.join(&s.name).to_string_lossy().to_string();
        if !cfg.roots.iter().any(|r| r.path == dir_str) {
            cfg.roots.push(config::KnowledgeRoot {
                path: dir_str,
                label: s.name.clone(),
                enabled: true,
                root_exclude_globs: vec![".git/".into()],
                root_exts: Vec::new(),
                root_max_file_kb: 0,
            });
            fixed.push(format!("补注册 git 源「{}」为知识根", s.name));
        }
    }
    // WebDAV 源：镜像缺失→移除源；根缺失→补注册；孤儿镜像根→移除
    let dav_root = webdav_dir(&app);
    let before = cfg.webdav_sources.len();
    cfg.webdav_sources
        .retain(|s| dav_root.join(&s.name).is_dir());
    if cfg.webdav_sources.len() < before {
        fixed.push(format!("移除镜像缺失的 WebDAV 源 {} 条", before - cfg.webdav_sources.len()));
    }
    cfg.roots.retain(|r| {
        let p = r.path.replace('\\', "/");
        if let Some(idx) = p.rfind("/webdav/") {
            let name = p[idx + 8..].trim_end_matches('/').to_string();
            if cfg.webdav_sources.iter().any(|s| s.name == name) {
                true
            } else {
                fixed.push(format!("移除孤儿镜像根「{}」", r.label));
                false
            }
        } else {
            true
        }
    });
    for s in &cfg.webdav_sources {
        let dir_str = dav_root.join(&s.name).to_string_lossy().to_string();
        if !cfg.roots.iter().any(|r| r.path == dir_str) {
            cfg.roots.push(config::KnowledgeRoot {
                path: dir_str,
                label: s.name.clone(),
                enabled: true,
                root_exclude_globs: Vec::new(),
                root_exts: Vec::new(),
                root_max_file_kb: 0,
            });
            fixed.push(format!("补注册 WebDAV 源「{}」为知识根", s.name));
        }
    }

    // 5. 值域回正
    if cfg.scope.max_file_kb == 0 {
        cfg.scope.max_file_kb = 4096;
        fixed.push("单文件大小上限回正为 4096 KB".into());
    }
    if cfg.scope.max_age_days > 36500 {
        cfg.scope.max_age_days = 0;
        fixed.push("时间窗异常值回正为不限".into());
    }
    let before = cfg.scope.include_exts.clone();
    cfg.scope.include_exts = cfg
        .scope
        .include_exts
        .iter()
        .map(|e| e.trim().trim_start_matches('.').to_lowercase())
        .filter(|e| !e.is_empty())
        .collect();
    if cfg.scope.include_exts != before {
        fixed.push("清洗扩展名列表".into());
    }

    // 有变更才落盘 + 重新同步
    if !fixed.is_empty() {
        // 同步索引到修复后的范围
        let roots = cfg.root_specs();
        let scope = cfg.scope.clone();
        *state.cfg.lock().expect("cfg lock") = cfg.clone();
        config::save(&app, &cfg);
        if !roots.is_empty() {
            let _ = state.engine.sync_roots(&roots, &scope);
        }
        start_watchers(&app);
    }
    Ok((fixed, cfg))
}

#[tauri::command]
fn rescan(_app: AppHandle, state: State<'_, AppState>) -> Result<Stats, String> {
    let roots = state.cfg.lock().expect("cfg lock").root_specs();
    if roots.is_empty() {
        return Err("尚未设置知识库目录".into());
    }
    let scope = state.cfg.lock().expect("cfg lock").scope.clone();
    state.engine.sync_roots(&roots, &scope)
}

/// 追加一个知识库目录（去重），保存配置、全量同步、重建监听
#[tauri::command]
fn pick_folder(app: AppHandle, state: State<'_, AppState>) -> Result<PickResult, String> {
    use tauri_plugin_dialog::DialogExt;

    // dialog 插件为回调式 API，用通道转同步；
    // 期间禁用"失焦隐藏"，避免父窗口隐藏连带对话框异常
    state.dialog_open.store(true, Ordering::SeqCst);
    let (tx, rx) = std::sync::mpsc::channel::<Option<tauri_plugin_dialog::FilePath>>();
    app.dialog().file().pick_folder(move |fp| {
        let _ = tx.send(fp);
    });
    let picked = rx.recv().map_err(|e| e.to_string());
    state.dialog_open.store(false, Ordering::SeqCst);
    let picked = picked?;

    let Some(fp) = picked else {
        // 用户取消
        return Ok(PickResult { dir: None, dirs: Vec::new(), stats: None, error: None });
    };
    let Some(path) = fp.as_path() else {
        return Ok(PickResult { dir: None, dirs: Vec::new(), stats: None, error: Some("路径无效".into()) });
    };
    let dir_str = path.to_string_lossy().to_string();

    {
        let mut cfg = state.cfg.lock().expect("cfg lock");
        if !cfg.roots.iter().any(|r| r.path == dir_str) {
            cfg.roots.push(config::KnowledgeRoot {
                path: dir_str.clone(),
                label: scope::basename_of(&dir_str),
                enabled: true,
                root_exclude_globs: Vec::new(),
                root_exts: Vec::new(),
                root_max_file_kb: 0,
            });
        }
        if cfg.knowledge_dir.is_none() {
            cfg.knowledge_dir = Some(dir_str.clone());
        }
        config::save(&app, &cfg);
    }

    let roots = state.cfg.lock().expect("cfg lock").root_specs();
    let scope = state.cfg.lock().expect("cfg lock").scope.clone();
    let stats = state.engine.sync_roots(&roots, &scope).ok();
    start_watchers(&app);
    let dirs = state
        .cfg
        .lock()
        .expect("cfg lock")
        .roots
        .iter()
        .map(|r| r.path.clone())
        .collect();
    Ok(PickResult { dir: Some(dir_str), dirs, stats, error: None })
}

/// 当前生效的同义词词典 JSON（默认 + 用户覆盖合并后）
#[tauri::command]
fn get_synonyms(state: State<'_, AppState>) -> String {
    let map = state.engine.synonyms_map();
    serde_json::to_string_pretty(&map).unwrap_or_else(|_| "{}".into())
}

/// 保存用户同义词词典：JSON 校验 → 写配置目录 → 引擎热重载
#[tauri::command]
fn save_synonyms(app: AppHandle, text: String) -> Result<(), String> {
    let map: HashMap<String, Vec<String>> = serde_json::from_str(&text)
        .map_err(|e| format!("JSON 格式错误: {e}"))?;
    let dir = app
        .path()
        .app_config_dir()
        .map_err(|e| e.to_string())?;
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    std::fs::write(dir.join("synonyms.json"), serde_json::to_string_pretty(&map).unwrap_or_default())
        .map_err(|e| e.to_string())?;
    // 热重载：用户文件覆盖默认词典（load_synonyms 会再做双向对称）
    let merged = engine::load_synonyms(Some(&dir));
    let state: State<AppState> = app.state();
    state.engine.reload_synonyms(merged);
    Ok(())
}


/// 当前生效的停用词列表 JSON
#[tauri::command]
fn get_stopwords(state: State<'_, AppState>) -> String {
    let set = state.engine.stopwords_set();
    let list: Vec<&String> = set.iter().collect();
    serde_json::to_string_pretty(&list).unwrap_or_else(|_| "[]".into())
}

/// 保存用户停用词列表（JSON 数组）→ 写配置目录 → 引擎热重载
#[tauri::command]
fn save_stopwords(app: AppHandle, text: String) -> Result<(), String> {
    let list: Vec<String> = serde_json::from_str(&text)
        .map_err(|e| format!("JSON 格式错误: {e}"))?;
    let dir = app.path().app_config_dir().map_err(|e| e.to_string())?;
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    std::fs::write(dir.join("stopwords.json"), serde_json::to_string_pretty(&list).unwrap_or_default())
        .map_err(|e| e.to_string())?;
    // 用户文件覆盖默认：直接用用户列表
    let set = list.iter().map(|w| w.trim().to_lowercase()).collect();
    let state: State<AppState> = app.state();
    state.engine.reload_stopwords(set);
    Ok(())
}


/// 保存当前配置为命名范围预设
#[tauri::command]
fn save_scope_preset(app: AppHandle, name: String, state: State<'_, AppState>) -> Result<(), String> {
    let mut cfg = state.cfg.lock().expect("cfg lock");
    let name = name.trim().to_string();
    if name.is_empty() { return Err("预设名不能为空".into()); }
    let roots = cfg.roots.clone();
    let excludes = cfg.scope.exclude_globs.clone();
    let includes = cfg.scope.include_globs.clone();
    let exts = cfg.scope.include_exts.clone();
    let max_age = cfg.scope.max_age_days;
    cfg.presets.retain(|p| p.name != name);
    cfg.presets.push(config::ScopePreset { name, roots, exclude_globs: excludes, include_exts: exts, include_globs: includes, max_age_days: max_age });
    let cfg_clone = cfg.clone();
    drop(cfg);
    config::save(&app, &cfg_clone);
    Ok(())
}

/// 加载范围预设（覆盖当前 roots + scope）
#[tauri::command]
fn load_scope_preset(app: AppHandle, name: String, state: State<'_, AppState>) -> Result<Config, String> {
    let mut cfg = state.cfg.lock().expect("cfg lock").clone();
    let preset = cfg.presets.iter().find(|p| p.name == name)
        .ok_or_else(|| format!("预设不存在: {name}"))?;
    cfg.roots = preset.roots.clone();
    cfg.scope.exclude_globs = preset.exclude_globs.clone();
    cfg.scope.include_globs = preset.include_globs.clone();
    cfg.scope.include_exts = preset.include_exts.clone();
    cfg.scope.max_age_days = preset.max_age_days;
    // 记录使用次数
    *cfg.scope_usage.entry(name).or_insert(0) += 1;
    // 写回全局状态：后续 rescan/搜索/保存都以此为准（此前只改克隆，
    // 后端仍用旧范围，预设"应用"实际不生效——v0.6.1 起的存量缺陷）
    *state.cfg.lock().expect("cfg lock") = cfg.clone();
    config::save(&app, &cfg);
    start_watchers(&app);
    let roots = cfg.root_specs();
    let scope = cfg.scope.clone();
    let _ = state.engine.sync_roots(&roots, &scope);
    Ok(cfg)
}

/// 删除范围预设
#[tauri::command]
fn delete_scope_preset(app: AppHandle, name: String, state: State<'_, AppState>) -> Result<(), String> {
    let mut cfg = state.cfg.lock().expect("cfg lock");
    cfg.presets.retain(|p| p.name != name);
    let cfg_clone = cfg.clone();
    drop(cfg);
    config::save(&app, &cfg_clone);
    Ok(())
}

/// 列出所有范围预设
#[tauri::command]
fn list_scope_presets(state: State<'_, AppState>) -> Vec<config::ScopePreset> {
    state.cfg.lock().expect("cfg lock").presets.clone()
}/// 导出配置到用户选择的 JSON 文件
#[tauri::command]
fn export_config(app: AppHandle, state: State<'_, AppState>) -> Result<String, String> {
    use tauri_plugin_dialog::DialogExt;
    let cfg = state.cfg.lock().expect("cfg lock").clone();
    let json = serde_json::to_string_pretty(&cfg).map_err(|e| e.to_string())?;
    state.dialog_open.store(true, Ordering::SeqCst);
    let (tx, rx) = std::sync::mpsc::channel::<Option<tauri_plugin_dialog::FilePath>>();
    app.dialog().file().set_file_name("knowledge-dock-config.json").save_file(move |fp| {
        let _ = tx.send(fp);
    });
    let picked = rx.recv().map_err(|e| e.to_string());
    state.dialog_open.store(false, Ordering::SeqCst);
    let picked = picked?;
    let Some(fp) = picked else { return Err("已取消".into()) };
    let Some(path) = fp.as_path() else { return Err("路径无效".into()) };
    std::fs::write(path, &json).map_err(|e| e.to_string())?;
    Ok(path.to_string_lossy().to_string())
}

/// 从用户选择的 JSON 文件导入配置（含快捷键换绑 + 多根重建）
#[tauri::command]
fn import_config(app: AppHandle, state: State<'_, AppState>) -> Result<Config, String> {
    use tauri_plugin_dialog::DialogExt;
    state.dialog_open.store(true, Ordering::SeqCst);
    let (tx, rx) = std::sync::mpsc::channel::<Option<tauri_plugin_dialog::FilePath>>();
    app.dialog().file().add_filter("JSON", &["json"]).pick_file(move |fp| {
        let _ = tx.send(fp);
    });
    let picked = rx.recv().map_err(|e| e.to_string());
    state.dialog_open.store(false, Ordering::SeqCst);
    let picked = picked?;
    let Some(fp) = picked else { return Err("已取消".into()) };
    let Some(path) = fp.as_path() else { return Err("路径无效".into()) };
    let json = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
    let cfg: Config = serde_json::from_str(&json).map_err(|e| format!("配置格式错误: {e}"))?;
    apply_shortcut(&app, &cfg.shortcut).map_err(|e| format!("快捷键注册失败: {e}"))?;
    *state.cfg.lock().expect("cfg lock") = cfg.clone();
    config::save(&app, &cfg);
    let roots = cfg.root_specs();
    if !roots.is_empty() {
        let scope = cfg.scope.clone();
        let _ = state.engine.sync_roots(&roots, &scope);
        start_watchers(&app);
    }
    Ok(cfg)
}

/// 就地展开上下文：读取源文件命中块前 N 行 + 后 M 行（不打开源文件）
/// 安全：仅允许读取任一知识库根目录内的文件
#[tauri::command]
fn expand_context(
    path: String,
    line: usize,
    before: usize,
    after: usize,
    state: State<'_, AppState>,
) -> Result<ContextResult, String> {
    let roots = state.cfg.lock().expect("cfg lock").root_specs();
    if roots.is_empty() {
        return Err("尚未设置知识库目录".into());
    }
    let canon_path = std::fs::canonicalize(&path).map_err(|e| e.to_string())?;
    let allowed = roots
        .iter()
        .filter_map(|spec| std::fs::canonicalize(&spec.path).ok())
        .any(|root| canon_path.starts_with(&root));
    if !allowed {
        return Err("路径不在知识库目录内".into());
    }

    let raw = std::fs::read(&canon_path).map_err(|e| e.to_string())?;
    let content = String::from_utf8_lossy(&raw);
    let lines: Vec<&str> = content.lines().collect();
    if lines.is_empty() {
        return Ok(ContextResult { content: String::new(), start_line: 0, end_line: 0, match_line: 0 });
    }
    let start = line.saturating_sub(before).max(1);
    let end = (line + after).min(lines.len());
    Ok(ContextResult {
        content: lines[start - 1..end].join("\n"),
        start_line: start,
        end_line: end,
        match_line: line,
    })
}

#[tauri::command]
fn open_file(path: String) -> Result<(), String> {
    open::that(&path).map_err(|e| e.to_string())
}

#[tauri::command]
fn reveal_file(path: String) -> Result<(), String> {
    std::process::Command::new("explorer")
        .arg(format!("/select,{}", path))
        .spawn()
        .map(|_| ())
        .map_err(|e| e.to_string())
}

#[tauri::command]
fn hide_window(app: AppHandle) {
    if let Some(win) = app.get_webview_window("main") {
        let _ = win.hide();
    }
}

#[tauri::command]
fn quit_app(app: AppHandle) {
    app.exit(0);
}

/// 前端初始化完成信号：此时 webview 已渲染，显示窗口（消除启动黑屏）
#[tauri::command]
fn frontend_ready(app: AppHandle) {
    if !FRONTEND_SHOWN.swap(true, Ordering::SeqCst) {
        reveal(&app);
    }
}

// ---------------- Git 仓库同步 ----------------

/// 克隆仓库的本地根目录
fn repos_dir(app: &AppHandle) -> PathBuf {
    app.path()
        .app_data_dir()
        .unwrap_or_else(|_| std::env::temp_dir().join("knowledge-dock"))
        .join("repos")
}

/// 记录某 git 源的同步错误（保存配置供 UI 展示）
fn git_record_error(app: &AppHandle, name: &str, err: &str) {
    let state: State<AppState> = app.state();
    let mut cfg = state.cfg.lock().expect("cfg lock");
    if let Some(s) = cfg.git_sources.iter_mut().find(|s| s.name == name) {
        s.last_error = Some(err.to_string());
    }
    config::save(app, &cfg);
}

/// 对单个 git 源执行浅拉取并同步索引（手动与定时共用）
/// 成功：更新 last_sync、清 last_error、保存配置、增量同步、emit index-updated
fn git_pull_one(app: &AppHandle, name: &str) -> Result<String, String> {
    let (branch, dir) = {
        let state: State<AppState> = app.state();
        let cfg = state.cfg.lock().expect("cfg lock");
        let src = cfg
            .git_sources
            .iter()
            .find(|s| s.name == name)
            .ok_or_else(|| format!("仓库不存在: {name}"))?;
        (src.branch.clone(), repos_dir(app).join(&src.name))
    };
    let state: State<AppState> = app.state();
    let _guard = state.git_lock.lock().expect("git lock");
    let msg = gitsync::pull_repo(&dir, &branch)?;
    // 成功：更新时间戳并触发索引同步（在 git 锁外做引擎同步，避免长持锁）
    let stats = {
        let mut cfg = state.cfg.lock().expect("cfg lock");
        if let Some(s) = cfg.git_sources.iter_mut().find(|s| s.name == name) {
            s.last_sync = gitsync::now_secs();
            s.last_error = None;
        }
        let roots = cfg.root_specs();
        let scope = cfg.scope.clone();
        config::save(app, &cfg);
        if roots.is_empty() {
            None
        } else {
            state.engine.sync_roots(&roots, &scope).ok()
        }
    };
    if let Some(st) = stats {
        let _ = app.emit("index-updated", &st);
    }
    Ok(msg)
}

/// 定时调度线程：每 30s 检查到期的远程源（git + WebDAV）并同步
fn start_git_scheduler(app: &AppHandle) {
    let handle = app.clone();
    std::thread::spawn(move || loop {
        std::thread::sleep(Duration::from_secs(30));
        sync_due_sources(&handle);
    });
}

/// 克隆仓库并注册为知识根（网络操作，异步防 UI 卡顿）
#[tauri::command]
async fn git_clone(
    app: AppHandle,
    url: String,
    branch: String,
    name: String,
    interval_min: u64,
) -> Result<Config, String> {
    tauri::async_runtime::spawn_blocking(move || git_clone_blocking(&app, url, branch, name, interval_min))
        .await
        .map_err(|e| format!("任务失败: {e}"))?
}

fn git_clone_blocking(
    app: &AppHandle,
    url: String,
    branch: String,
    name: String,
    interval_min: u64,
) -> Result<Config, String> {
    let url = url.trim().to_string();
    if url.is_empty() {
        return Err("仓库 URL 不能为空".into());
    }
    // 显示名：留空则从 URL 推导
    let name = if name.trim().is_empty() {
        gitsync::name_from_url(&url)
    } else {
        name.trim().to_string()
    };
    let name = gitsync::sanitize_name(&name).ok_or("显示名只能包含字母/数字/-/_")?;

    let state: State<AppState> = app.state();
    {
        let cfg = state.cfg.lock().expect("cfg lock");
        if cfg.git_sources.iter().any(|s| s.name == name) {
            return Err(format!("同名仓库已存在: {name}"));
        }
    }

    let dir = repos_dir(app).join(&name);
    let _guard = state.git_lock.lock().expect("git lock");
    gitsync::clone_repo(&url, &branch, &dir)?;
    // 分支留空时回填远端默认分支
    let branch = if branch.trim().is_empty() {
        gitsync::current_branch(&dir)?
    } else {
        branch.trim().to_string()
    };
    drop(_guard);

    let stats = {
        let mut cfg = state.cfg.lock().expect("cfg lock");
        let dir_str = dir.to_string_lossy().to_string();
        cfg.git_sources.push(config::GitRepoSource {
            name: name.clone(),
            url: url.clone(),
            branch: branch.clone(),
            enabled: true,
            interval_min,
            last_sync: gitsync::now_secs(),
            last_error: None,
        });
        // 注册为知识根：.git 目录独立排除（扫描层另有 . 开头目录兜底）
        if !cfg.roots.iter().any(|r| r.path == dir_str) {
            cfg.roots.push(config::KnowledgeRoot {
                path: dir_str,
                label: name.clone(),
                enabled: true,
                root_exclude_globs: vec![".git/".into()],
                root_exts: Vec::new(),
                root_max_file_kb: 0,
            });
        }
        let roots = cfg.root_specs();
        let scope = cfg.scope.clone();
        config::save(app, &cfg);
        if roots.is_empty() {
            None
        } else {
            state.engine.sync_roots(&roots, &scope).ok()
        }
    };
    start_watchers(app);
    if let Some(st) = stats {
        let _ = app.emit("index-updated", &st);
    }
    let cfg_out = state.cfg.lock().expect("cfg lock").clone();
    Ok(cfg_out)
}

/// 手动拉取某仓库更新
#[tauri::command]
async fn git_pull(app: AppHandle, name: String) -> Result<Config, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let result = git_pull_one(&app, &name);
        let state: State<AppState> = app.state();
        match result {
            Ok(_msg) => {
                let cfg_out = state.cfg.lock().expect("cfg lock").clone();
                Ok(cfg_out)
            }
            Err(e) => {
                git_record_error(&app, &name, &e);
                Err(e)
            }
        }
    })
    .await
    .map_err(|e| format!("任务失败: {e}"))?
}

/// 移除 git 源（连带删除克隆目录与对应知识根，重新同步索引）
#[tauri::command]
async fn git_remove(app: AppHandle, name: String) -> Result<Config, String> {
    tauri::async_runtime::spawn_blocking(move || git_remove_blocking(&app, name))
        .await
        .map_err(|e| format!("任务失败: {e}"))?
}

fn git_remove_blocking(app: &AppHandle, name: String) -> Result<Config, String> {
    let state: State<AppState> = app.state();
    {
        let mut cfg = state.cfg.lock().expect("cfg lock");
        let before = cfg.git_sources.len();
        cfg.git_sources.retain(|s| s.name != name);
        if cfg.git_sources.len() == before {
            return Err(format!("仓库不存在: {name}"));
        }
        let dir = repos_dir(app).join(&name);
        cfg.roots
            .retain(|r| r.path != dir.to_string_lossy().to_string());
        let roots = cfg.root_specs();
        let scope = cfg.scope.clone();
        config::save(app, &cfg);
        if !roots.is_empty() {
            let _ = state.engine.sync_roots(&roots, &scope);
        }
    }
    start_watchers(app);
    // 删除克隆目录（best-effort，失败不影响配置移除）
    let _ = gitsync::remove_repo_dir(&repos_dir(app), &name);
    let cfg_out = state.cfg.lock().expect("cfg lock").clone();
    Ok(cfg_out)
}

// ---------------- WebDAV 远程目录源 ----------------

/// WebDAV 镜像根目录
fn webdav_dir(app: &AppHandle) -> PathBuf {
    app.path()
        .app_data_dir()
        .unwrap_or_else(|_| std::env::temp_dir().join("knowledge-dock"))
        .join("webdav")
}

/// WebDAV 源镜像元数据文件
fn webdav_meta(app: &AppHandle, name: &str) -> PathBuf {
    webdav_dir(app).join(format!("{name}.meta.json"))
}

fn load_mirror(app: &AppHandle, name: &str) -> webdav::MirrorState {
    std::fs::read_to_string(webdav_meta(app, name))
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

fn save_mirror(app: &AppHandle, name: &str, state_map: &webdav::MirrorState) {
    let _ = std::fs::create_dir_all(webdav_dir(app));
    if let Ok(json) = serde_json::to_string_pretty(state_map) {
        let _ = std::fs::write(webdav_meta(app, name), json);
    }
}

fn webdav_record_error(app: &AppHandle, name: &str, err: &str) {
    let state: State<AppState> = app.state();
    let mut cfg = state.cfg.lock().expect("cfg lock");
    if let Some(s) = cfg.webdav_sources.iter_mut().find(|s| s.name == name) {
        s.last_error = Some(err.to_string());
    }
    config::save(app, &cfg);
}

/// 对单个 WebDAV 源执行镜像同步（手动与定时共用）
fn webdav_sync_one(app: &AppHandle, name: &str) -> Result<String, String> {
    let (url, username, password) = {
        let state: State<AppState> = app.state();
        let cfg = state.cfg.lock().expect("cfg lock");
        let src = cfg
            .webdav_sources
            .iter()
            .find(|s| s.name == name)
            .ok_or_else(|| format!("源不存在: {name}"))?;
        (src.url.clone(), src.username.clone(), src.password.clone())
    };
    let state: State<AppState> = app.state();
    let _guard = state.webdav_lock.lock().expect("webdav lock");

    let remote = webdav::list_remote(&url, &username, &password)?;
    let local_root = webdav_dir(app).join(name);
    std::fs::create_dir_all(&local_root).map_err(|e| e.to_string())?;
    let mut mirror = load_mirror(app, name);
    let max_kb = {
        let cfg = state.cfg.lock().expect("cfg lock");
        cfg.scope.max_file_kb
    };
    let (to_download, to_delete) = webdav::diff_mirror(&remote, &mirror, &local_root, max_kb);

    let mut downloaded = 0usize;
    let mut deleted = 0usize;
    for (rf, local) in &to_download {
        if let Some(parent) = local.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let file_url = format!("{}/{}", url.trim_end_matches('/'), rf.rel);
        webdav::download(&file_url, &username, &password, local)?;
        downloaded += 1;
    }
    for rel in &to_delete {
        let local = local_root.join(rel.replace('/', "\\"));
        if local.is_file() {
            let _ = std::fs::remove_file(&local);
            deleted += 1;
        }
    }
    // 更新镜像元数据
    mirror = remote
        .iter()
        .map(|r| (r.rel.clone(), r.clone()))
        .collect();
    save_mirror(app, name, &mirror);

    // 触发索引增量同步（镜像目录已是知识根）
    let stats = {
        let mut cfg = state.cfg.lock().expect("cfg lock");
        if let Some(s) = cfg.webdav_sources.iter_mut().find(|s| s.name == name) {
            s.last_sync = gitsync::now_secs();
            s.last_error = None;
        }
        let roots = cfg.root_specs();
        let scope = cfg.scope.clone();
        config::save(app, &cfg);
        if roots.is_empty() {
            None
        } else {
            state.engine.sync_roots(&roots, &scope).ok()
        }
    };
    if let Some(st) = stats {
        let _ = app.emit("index-updated", &st);
    }
    Ok(format!("下载 {downloaded}，删除 {deleted}"))
}

/// 定时调度扩展：git 源 + WebDAV 源统一检查
fn sync_due_sources(app: &AppHandle) {
    let now = gitsync::now_secs();
    let git_due: Vec<String> = {
        let state: State<AppState> = app.state();
        let cfg = match state.cfg.lock() {
            Ok(c) => c,
            Err(_) => return,
        };
        cfg.git_sources
            .iter()
            .filter(|s| {
                s.enabled
                    && s.interval_min > 0
                    && (s.last_sync == 0 || now.saturating_sub(s.last_sync) >= s.interval_min * 60)
            })
            .map(|s| s.name.clone())
            .collect()
    };
    for name in git_due {
        if let Err(e) = git_pull_one(app, &name) {
            git_record_error(app, &name, &e);
            let _ = app.emit("git-sync-error", serde_json::json!({ "name": name, "error": e }));
        }
    }
    let dav_due: Vec<String> = {
        let state: State<AppState> = app.state();
        let cfg = match state.cfg.lock() {
            Ok(c) => c,
            Err(_) => return,
        };
        cfg.webdav_sources
            .iter()
            .filter(|s| {
                s.enabled
                    && s.interval_min > 0
                    && (s.last_sync == 0 || now.saturating_sub(s.last_sync) >= s.interval_min * 60)
            })
            .map(|s| s.name.clone())
            .collect()
    };
    for name in dav_due {
        if let Err(e) = webdav_sync_one(app, &name) {
            webdav_record_error(app, &name, &e);
        }
    }
}

/// 测试 WebDAV 连接（不落盘）
#[tauri::command]
async fn webdav_test(url: String, username: String, password: String) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || webdav::test_connection(&url, &username, &password))
        .await
        .map_err(|e| format!("任务失败: {e}"))?
}

/// 添加 WebDAV 源：测试连接 → 首次镜像同步 → 注册知识根
#[tauri::command]
async fn webdav_add(
    app: AppHandle,
    url: String,
    username: String,
    password: String,
    name: String,
    interval_min: u64,
) -> Result<Config, String> {
    tauri::async_runtime::spawn_blocking(move || {
        webdav_add_blocking(&app, url, username, password, name, interval_min)
    })
    .await
    .map_err(|e| format!("任务失败: {e}"))?
}

fn webdav_add_blocking(
    app: &AppHandle,
    url: String,
    username: String,
    password: String,
    name: String,
    interval_min: u64,
) -> Result<Config, String> {
    let url = url.trim().to_string();
    if url.is_empty() {
        return Err("WebDAV URL 不能为空".into());
    }
    let name = if name.trim().is_empty() {
        gitsync::name_from_url(&url)
    } else {
        name.trim().to_string()
    };
    let name = gitsync::sanitize_name(&name).ok_or("显示名只能包含字母/数字/-/_")?;

    let state: State<AppState> = app.state();
    {
        let cfg = state.cfg.lock().expect("cfg lock");
        if cfg.webdav_sources.iter().any(|s| s.name == name) {
            return Err(format!("同名源已存在: {name}"));
        }
    }

    let _guard = state.webdav_lock.lock().expect("webdav lock");
    webdav::test_connection(&url, &username, &password)?;

    // 先做首次镜像同步，成功后才写入配置（失败不留残骸）
    let remote = webdav::list_remote(&url, &username, &password).map_err(|e| {
        format!("枚举远端失败: {e}")
    })?;
    let local_root = webdav_dir(app).join(&name);
    std::fs::create_dir_all(&local_root).map_err(|e| format!("创建镜像目录失败: {e}"))?;
    let max_kb = {
        let cfg = state.cfg.lock().expect("cfg lock");
        cfg.scope.max_file_kb
    };
    let (to_download, _to_delete) = webdav::diff_mirror(&remote, &Default::default(), &local_root, max_kb);
    for (rf, local) in &to_download {
        if let Some(parent) = local.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let file_url = format!("{}/{}", url.trim_end_matches('/'), rf.rel);
        webdav::download(&file_url, &username, &password, local)
            .map_err(|e| format!("下载 {} 失败: {e}", rf.rel))?;
    }
    let mirror: webdav::MirrorState = remote.iter().map(|r| (r.rel.clone(), r.clone())).collect();
    save_mirror(app, &name, &mirror);

    // 同步成功：写入配置 + 注册知识根
    let mut cfg = state.cfg.lock().expect("cfg lock").clone();
    cfg.webdav_sources.push(config::WebdavSource {
        name: name.clone(),
        url: url.clone(),
        username: username.trim().to_string(),
        password: password.to_string(),
        enabled: true,
        interval_min,
        last_sync: gitsync::now_secs(),
        last_error: None,
    });
    let dir_str = local_root.to_string_lossy().to_string();
    if !cfg.roots.iter().any(|r| r.path == dir_str) {
        cfg.roots.push(config::KnowledgeRoot {
            path: dir_str,
            label: name.clone(),
            enabled: true,
            root_exclude_globs: Vec::new(),
            root_exts: Vec::new(),
            root_max_file_kb: 0,
        });
    }
    *state.cfg.lock().expect("cfg lock") = cfg.clone();
    config::save(app, &cfg);
    drop(_guard);

    // 索引增量同步
    {
        let roots = cfg.root_specs();
        let scope = cfg.scope.clone();
        if !roots.is_empty() {
            let _ = state.engine.sync_roots(&roots, &scope);
        }
    }
    start_watchers(app);
    let cfg_out = state.cfg.lock().expect("cfg lock").clone();
    Ok(cfg_out)
}

/// 手动同步 WebDAV 源
#[tauri::command]
async fn webdav_pull(app: AppHandle, name: String) -> Result<Config, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let result = webdav_sync_one(&app, &name);
        let state: State<AppState> = app.state();
        match result {
            Ok(_msg) => {
                let cfg_out = state.cfg.lock().expect("cfg lock").clone();
                Ok(cfg_out)
            }
            Err(e) => {
                webdav_record_error(&app, &name, &e);
                Err(e)
            }
        }
    })
    .await
    .map_err(|e| format!("任务失败: {e}"))?
}

/// 移除 WebDAV 源（连带移除知识根与本地镜像）
#[tauri::command]
async fn webdav_remove(app: AppHandle, name: String) -> Result<Config, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let state: State<AppState> = app.state();
        {
            let mut cfg = state.cfg.lock().expect("cfg lock");
            let before = cfg.webdav_sources.len();
            cfg.webdav_sources.retain(|s| s.name != name);
            if cfg.webdav_sources.len() == before {
                return Err(format!("源不存在: {name}"));
            }
            let dir = webdav_dir(&app).join(&name);
            cfg.roots
                .retain(|r| r.path != dir.to_string_lossy().to_string());
            let roots = cfg.root_specs();
            let scope = cfg.scope.clone();
            config::save(&app, &cfg);
            *state.cfg.lock().expect("cfg lock") = cfg.clone();
            if !roots.is_empty() {
                let _ = state.engine.sync_roots(&roots, &scope);
            }
        }
        start_watchers(&app);
        let _ = std::fs::remove_dir_all(webdav_dir(&app).join(&name));
        let _ = std::fs::remove_file(webdav_meta(&app, &name));
        let cfg_out = state.cfg.lock().expect("cfg lock").clone();
        Ok(cfg_out)
    })
    .await
    .map_err(|e| format!("任务失败: {e}"))?
}

// ---------------- 位置记忆 ----------------

fn save_position_throttled(app: &AppHandle, x: i32, y: i32) {
    LAST_X.store(x as i64, Ordering::Relaxed);
    LAST_Y.store(y as i64, Ordering::Relaxed);
    let now = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    let last = LAST_SAVE_MS.load(Ordering::Relaxed);
    if now.saturating_sub(last) < 800 {
        return;
    }
    LAST_SAVE_MS.store(now, Ordering::Relaxed);
    let state: State<AppState> = app.state();
    let mut cfg = state.cfg.lock().expect("cfg lock").clone();
    cfg.x = Some(x);
    cfg.y = Some(y);
    config::save(app, &cfg);
}

fn save_position_final(app: &AppHandle) {
    let (x, y) = (LAST_X.load(Ordering::Relaxed), LAST_Y.load(Ordering::Relaxed));
    if x == i64::MIN {
        return;
    }
    let state: State<AppState> = app.state();
    let mut cfg = state.cfg.lock().expect("cfg lock").clone();
    cfg.x = Some(x as i32);
    cfg.y = Some(y as i32);
    config::save(app, &cfg);
}

/// 保存的位置是否落在某个显示器范围内
fn position_on_screen(app: &AppHandle, x: f64, y: f64) -> bool {
    if let Ok(monitors) = app.available_monitors() {
        for m in monitors {
            let mpx = m.position().x as f64;
            let mpy = m.position().y as f64;
            let mw = m.size().width as f64;
            let mh = m.size().height as f64;
            if x >= mpx && x < mpx + mw && y >= mpy && y < mpy + mh {
                return true;
            }
        }
    }
    false
}

// ---------------- 入口 ----------------

pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            reveal(app);
        }))
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_global_shortcut::Builder::new().build())
        .setup(|app| {
            let handle = app.handle().clone();
            let cfg = config::load(&handle);

            // 检索引擎：打开持久化索引（秒开），同义词 = 内嵌默认 + 用户覆盖
            let data_dir = handle
                .path()
                .app_data_dir()
                .unwrap_or_else(|_| std::env::temp_dir().join("knowledge-dock"));
            let stopwords = engine::load_stopwords(
                handle.path().app_config_dir().ok().as_deref(),
            );
            let synonyms = engine::load_synonyms(
                handle.path().app_config_dir().ok().as_deref(),
            );
            let engine = Arc::new(Engine::open_or_create(&data_dir.join("index"), synonyms, stopwords)?);

            // 先注册全局状态（后续 setup 内的所有 state() 访问都依赖它）
            app.manage(AppState {
                engine,
                cfg: Mutex::new(cfg.clone()),
                dialog_open: AtomicBool::new(false),
                watcher: Mutex::new(None),
                git_lock: Mutex::new(()),
                webdav_lock: Mutex::new(()),
            });

            // 恢复窗口位置与宽度（越界则保持默认居中）
            if let Some(win) = handle.get_webview_window("main") {
                let _ = win.set_size(tauri::LogicalSize::new(cfg.width, COLLAPSED_H));
            }
            if let (Some(x), Some(y)) = (cfg.x, cfg.y) {
                if position_on_screen(&handle, x as f64, y as f64) {
                    if let Some(win) = handle.get_webview_window("main") {
                        let _ = win.set_position(tauri::PhysicalPosition::new(x, y));
                        LAST_X.store(x as i64, Ordering::Relaxed);
                        LAST_Y.store(y as i64, Ordering::Relaxed);
                    }
                }
            }

            // 系统托盘
            let show = MenuItem::with_id(&handle, "show", "显示窗口", true, None::<&str>)?;
            let quit = MenuItem::with_id(&handle, "quit", "退出", true, None::<&str>)?;
            let menu = Menu::with_items(&handle, &[&show, &quit])?;
            TrayIconBuilder::with_id("kd-tray")
                .icon(handle.default_window_icon().expect("app icon").clone())
                .tooltip("知识悬浮搜索")
                .menu(&menu)
                .show_menu_on_left_click(false)
                .on_menu_event(|app, ev| match ev.id.as_ref() {
                    "show" => reveal(app),
                    "quit" => {
                        save_position_final(app);
                        app.exit(0);
                    }
                    _ => {}
                })
                .build(&handle)?;

            // 全局快捷键（注册失败则回退默认键）
            if apply_shortcut(&handle, &cfg.shortcut).is_err() {
                let _ = apply_shortcut(&handle, config::DEFAULT_SHORTCUT);
            }

            // 后台校验/构建索引（已建好则 stat 秒过），并挂多根文件监听
            if !cfg.effective_roots().is_empty() {
                sync_and_emit(&handle);
                start_watchers(&handle);
            }

            // git 仓库定时更新调度（每 30s 检查到期源）
            start_git_scheduler(&handle);

            // 兜底：前端异常（如脚本崩溃）无法发出 frontend_ready 时，
            // 3 秒后强制显示窗口，避免应用永远不可见
            let bg = handle.clone();
            std::thread::spawn(move || {
                std::thread::sleep(Duration::from_secs(3));
                if !FRONTEND_SHOWN.swap(true, Ordering::SeqCst) {
                    reveal(&bg);
                }
            });

            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            get_config,
            save_config,
            ui_mode,
            search,
            add_search_history,
            get_search_history,
            clear_search_history,
            get_index_stats,
            scope_check,
            health_check,
            health_fix_all,
            import_scope_rules,
            export_scope_rules,
            load_config_profile,
            list_config_profiles,
            save_config_profile,
            apply_scope_template,
            get_scope_templates,
            rescan,
            pick_folder,
            expand_context,
            get_synonyms,
            save_synonyms,
            get_stopwords,
            save_stopwords,
            save_scope_preset,
            load_scope_preset,
            delete_scope_preset,
            list_scope_presets,
            export_config,
            import_config,
            open_file,
            reveal_file,
            hide_window,
            frontend_ready,
            git_clone,
            git_pull,
            git_remove,
            webdav_test,
            webdav_add,
            webdav_pull,
            webdav_remove,
            quit_app
        ])
        .on_window_event(|window, event| match event {
            WindowEvent::CloseRequested { api, .. } => {
                // 点关闭 = 隐藏
                api.prevent_close();
                let _ = window.hide();
            }
            WindowEvent::Focused(false) => {
                // 失焦自动隐藏（钉住模式与原生对话框打开期间除外）
                let state: State<AppState> = window.app_handle().state();
                let pinned = state.cfg.lock().expect("cfg lock").pin_mode;
                if !pinned && !state.dialog_open.load(Ordering::SeqCst) {
                    let _ = window.hide();
                }
            }
            WindowEvent::Moved(pos) => {
                save_position_throttled(window.app_handle(), pos.x, pos.y);
            }
            _ => {}
        })
        .build(tauri::generate_context!())
        .expect("error while building tauri application")
        .run(|app, event| {
            if let tauri::RunEvent::Exit = event {
                save_position_final(app);
            }
        });
}
