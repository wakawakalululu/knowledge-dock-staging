//! 知识悬浮搜索 —— uTools 风格的本地知识库常驻置顶检索窗（逻辑主体）
//!
//! 形态：折叠时仅一条搜索框高度，检索时向下展开结果面板；
//! 全局快捷键唤起/隐藏，失焦自动隐藏（钉住模式除外）。
//! 检索：Tantivy BM25 + jieba 分词 + 持久化索引 + 同义词改写（纯离线）。

pub mod config;
pub mod scope;
pub mod engine;
pub mod indexer;

#[cfg(test)]
mod search_tests;

use config::Config;
use engine::{Engine, SearchResponse, StatsDetail};
use indexer::Stats;
use notify::{RecommendedWatcher, RecursiveMode, Watcher};
use serde::Serialize;
use std::collections::HashMap;
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
}

/// 拖动节流：窗口移动事件的保存间隔
static LAST_SAVE_MS: AtomicU64 = AtomicU64::new(0);
/// 最新窗口位置（物理像素），供退出时兜底保存
static LAST_X: AtomicI64 = AtomicI64::new(i64::MIN);
static LAST_Y: AtomicI64 = AtomicI64::new(i64::MIN);

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
    let roots = state.cfg.lock().expect("cfg lock").root_pairs();
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
    let roots = state.cfg.lock().expect("cfg lock").root_pairs();
    if roots.is_empty() {
        return;
    }
    let (tx, rx) = std::sync::mpsc::channel();
    let mut watcher = match notify::recommended_watcher(tx) {
        Ok(w) => w,
        Err(_) => return,
    };
    for (r, _label) in &roots {
        if watcher.watch(r, RecursiveMode::Recursive).is_err() {
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
    state: State<'_, AppState>,
) -> SearchResponse {
    state
        .engine
        .search(&q, 50, type_filter.as_deref(), root_filter.as_deref())
}

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

/// 索引统计仪表盘：各类型/各库块数分布
#[tauri::command]
fn get_index_stats(state: State<'_, AppState>) -> StatsDetail {
    state.engine.stats_detail()
}

#[tauri::command]
fn rescan(_app: AppHandle, state: State<'_, AppState>) -> Result<Stats, String> {
    let roots = state.cfg.lock().expect("cfg lock").root_pairs();
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
            });
        }
        if cfg.knowledge_dir.is_none() {
            cfg.knowledge_dir = Some(dir_str.clone());
        }
        config::save(&app, &cfg);
    }

    let roots = state.cfg.lock().expect("cfg lock").root_pairs();
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
    let mut set = engine::load_stopwords(Some(&dir));
    // 用户文件覆盖默认：直接用用户列表
    set = list.iter().map(|w| w.trim().to_lowercase()).collect();
    let state: State<AppState> = app.state();
    state.engine.reload_stopwords(set);
    Ok(())
}
/// 导出配置到用户选择的 JSON 文件
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
    let roots = cfg.root_pairs();
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
    let roots = state.cfg.lock().expect("cfg lock").root_pairs();
    if roots.is_empty() {
        return Err("尚未设置知识库目录".into());
    }
    let canon_path = std::fs::canonicalize(&path).map_err(|e| e.to_string())?;
    let allowed = roots
        .iter()
        .filter_map(|(r, _)| std::fs::canonicalize(r).ok())
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
            rescan,
            pick_folder,
            expand_context,
            get_synonyms,
            save_synonyms,
            get_stopwords,
            save_stopwords,
            export_config,
            import_config,
            open_file,
            reveal_file,
            hide_window,
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
