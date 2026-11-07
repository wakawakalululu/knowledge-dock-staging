//! 知识悬浮搜索 —— uTools 风格的本地知识库常驻置顶检索窗（逻辑主体）
//!
//! 形态：折叠时仅一条搜索框高度，检索时向下展开结果面板；
//! 全局快捷键唤起/隐藏，失焦自动隐藏（钉住模式除外）。
//! 检索：Tantivy BM25 + jieba 分词 + 持久化索引 + 同义词改写（纯离线）。

pub mod config;
pub mod engine;
pub mod indexer;

#[cfg(test)]
mod search_tests;

use config::Config;
use engine::{Engine, SearchResponse};
use indexer::Stats;
use notify::{RecursiveMode, Watcher};
use serde::Serialize;
use std::path::{Path, PathBuf};
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
}

/// 拖动节流：窗口移动事件的保存间隔
static LAST_SAVE_MS: AtomicU64 = AtomicU64::new(0);
/// 最新窗口位置（物理像素），供退出时兜底保存
static LAST_X: AtomicI64 = AtomicI64::new(i64::MIN);
static LAST_Y: AtomicI64 = AtomicI64::new(i64::MIN);

#[derive(Serialize)]
struct PickResult {
    dir: Option<String>,
    stats: Option<Stats>,
    error: Option<String>,
}

/// 后台执行一次同步并推送统计（首次构建/校验/增量复用同一入口）
fn sync_and_emit(app: &AppHandle, dir: &Path) {
    let state: State<AppState> = app.state();
    match state.engine.sync(dir) {
        Ok(stats) => {
            let _ = app.emit("index-updated", &stats);
        }
        Err(e) => {
            let _ = app.emit("index-error", e);
        }
    }
}

/// 启动/重启文件监听线程：事件静默 900ms 后增量更新索引并推送前端
fn start_watcher(app: &AppHandle, dir: PathBuf) {
    let handle = app.clone();
    std::thread::spawn(move || {
        let (tx, rx) = std::sync::mpsc::channel();
        let Ok(mut watcher) = notify::recommended_watcher(tx) else { return };
        if watcher.watch(&dir, RecursiveMode::Recursive).is_err() {
            return;
        }
        loop {
            // 阻塞等待首个事件；通道关闭则退出线程
            if rx.recv().is_err() {
                return;
            }
            // 持续吸收事件，直到静默 900ms
            loop {
                match rx.recv_timeout(Duration::from_millis(900)) {
                    Ok(_) => continue,
                    Err(_) => break,
                }
            }
            let state = handle.state::<AppState>();
            let dir_now = state.cfg.lock().expect("cfg lock").knowledge_dir.clone();
            if let Some(d) = dir_now {
                sync_and_emit(&handle, Path::new(&d));
            }
        }
    });
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
fn search(q: String, state: State<'_, AppState>) -> SearchResponse {
    state.engine.search(&q, 50)
}

#[tauri::command]
fn rescan(_app: AppHandle, state: State<'_, AppState>) -> Result<Stats, String> {
    let dir = state
        .cfg
        .lock()
        .expect("cfg lock")
        .knowledge_dir
        .clone()
        .ok_or("尚未设置知识库目录")?;
    state.engine.sync(Path::new(&dir))
}

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
        return Ok(PickResult { dir: None, stats: None, error: None });
    };
    let Some(path) = fp.as_path() else {
        return Ok(PickResult { dir: None, stats: None, error: Some("路径无效".into()) });
    };
    let dir_str = path.to_string_lossy().to_string();

    match state.engine.sync(path) {
        Ok(stats) => {
            {
                let mut cfg = state.cfg.lock().expect("cfg lock");
                cfg.knowledge_dir = Some(dir_str.clone());
                config::save(&app, &cfg);
            }
            start_watcher(&app, path.to_path_buf());
            Ok(PickResult { dir: Some(dir_str), stats: Some(stats), error: None })
        }
        Err(e) => Ok(PickResult { dir: None, stats: None, error: Some(e) }),
    }
}

/// 就地展开上下文：读取源文件命中块前 N 行 + 后 M 行（不打开源文件）
#[tauri::command]
fn expand_context(path: String, line: usize, before: usize, after: usize) -> Result<String, String> {
    let raw = std::fs::read(&path).map_err(|e| e.to_string())?;
    let content = String::from_utf8_lossy(&raw);
    let lines: Vec<&str> = content.lines().collect();
    if lines.is_empty() {
        return Ok(String::new());
    }
    let start = line.saturating_sub(before).max(1);
    let end = (line + after).min(lines.len());
    Ok(lines[start - 1..end].join("\n"))
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
            let synonyms = engine::load_synonyms(
                handle.path().app_config_dir().ok().as_deref(),
            );
            let engine = Arc::new(Engine::open_or_create(&data_dir.join("index"), synonyms)?);

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

            // 后台校验/构建索引（已建好则秒过），并挂文件监听
            if let Some(dir) = cfg.knowledge_dir.clone() {
                let h2 = handle.clone();
                let d2 = dir.clone();
                std::thread::spawn(move || {
                    sync_and_emit(&h2, Path::new(&d2));
                });
                start_watcher(&handle, PathBuf::from(&dir));
            }

            app.manage(AppState {
                engine,
                cfg: Mutex::new(cfg),
                dialog_open: AtomicBool::new(false),
            });
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            get_config,
            save_config,
            ui_mode,
            search,
            rescan,
            pick_folder,
            expand_context,
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
