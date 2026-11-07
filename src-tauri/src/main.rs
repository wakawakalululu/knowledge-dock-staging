#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

//! 知识悬浮搜索 —— uTools 风格的本地知识库常驻置顶检索窗

mod config;
mod indexer;

use config::Config;
use indexer::{Hit, Index, Stats};
use notify::{RecursiveMode, Watcher};
use serde::Serialize;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};
use tauri::menu::{Menu, MenuItem};
use tauri::tray::TrayIconBuilder;
use tauri::{AppHandle, Emitter, Manager, State, WindowEvent};

struct AppState {
    index: Mutex<Index>,
    cfg: Mutex<Config>,
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

fn rebuild_index(app: &AppHandle, state: &State<'_, AppState>, dir: &Path) -> Result<Stats, String> {
    let index = Index::build(dir)?;
    let stats = index.stats();
    *state.index.lock().expect("index lock") = index;
    let _ = app.emit("index-updated", &stats);
    Ok(stats)
}

/// 启动/重启文件监听线程：事件静默 900ms 后重建索引并推送前端
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
            let dir_now = state
                .cfg
                .lock()
                .expect("cfg lock")
                .knowledge_dir
                .clone();
            if let Some(d) = dir_now {
                let _ = rebuild_index(&handle, &state, Path::new(&d));
            }
        }
    });
}

fn show_main(app: &AppHandle) {
    if let Some(win) = app.get_webview_window("main") {
        let _ = win.show();
        let _ = win.set_focus();
    }
}

// ---------------- Tauri 命令 ----------------

#[tauri::command]
fn get_config(state: State<'_, AppState>) -> Config {
    state.cfg.lock().expect("cfg lock").clone()
}

#[tauri::command]
fn search(q: String, state: State<'_, AppState>) -> Vec<Hit> {
    state.index.lock().expect("index lock").search(&q)
}

#[tauri::command]
fn rescan(app: AppHandle, state: State<'_, AppState>) -> Result<Stats, String> {
    let dir = state
        .cfg
        .lock()
        .expect("cfg lock")
        .knowledge_dir
        .clone()
        .ok_or("尚未设置知识库目录")?;
    rebuild_index(&app, &state, Path::new(&dir))
}

#[tauri::command]
async fn pick_folder(app: AppHandle, state: State<'_, AppState>) -> Result<PickResult, String> {
    use tauri_plugin_dialog::DialogExt;

    let picked = app.dialog().file().pick_folder().await;
    let Some(fp) = picked else {
        // 用户取消
        return Ok(PickResult { dir: None, stats: None, error: None });
    };
    let Some(path) = fp.as_path() else {
        return Ok(PickResult { dir: None, stats: None, error: Some("路径无效".into()) });
    };
    let dir_str = path.to_string_lossy().to_string();

    let result = rebuild_index(&app, &state, path);
    match result {
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
    let now = Instant::now()
        .duration_since(Instant::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    let last = LAST_SAVE_MS.load(Ordering::Relaxed);
    if now.saturating_sub(last) < 800 {
        return;
    }
    LAST_SAVE_MS.store(now, Ordering::Relaxed);
    let state: State<AppState> = app.state();
    let cfg = state.cfg.lock().expect("cfg lock");
    let mut cfg = cfg.clone();
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

fn main() {
    tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            show_main(app);
        }))
        .plugin(tauri_plugin_dialog::init())
        .setup(|app| {
            let handle = app.handle().clone();
            let cfg = config::load(&handle);

            // 恢复窗口位置（越界则保持默认居中）
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
            let show = MenuItem::with_id(handle, "show", "显示窗口", true, None::<&str>)?;
            let quit = MenuItem::with_id(handle, "quit", "退出", true, None::<&str>)?;
            let menu = Menu::with_items(handle, &[&show, &quit])?;
            TrayIconBuilder::with_id("kd-tray")
                .icon(handle.default_window_icon().expect("app icon").clone())
                .tooltip("知识悬浮搜索")
                .menu(&menu)
                .show_menu_on_left_click(false)
                .on_menu_event(|app, ev| match ev.id.as_ref() {
                    "show" => show_main(app),
                    "quit" => {
                        save_position_final(app);
                        app.exit(0);
                    }
                    _ => {}
                })
                .build(handle)?;

            // 启动时若已有知识库目录则后台建索引 + 监听
            if let Some(dir) = cfg.knowledge_dir.clone() {
                let state: State<AppState> = handle.state();
                let _ = rebuild_index(&handle, &state, Path::new(&dir));
                start_watcher(&handle, PathBuf::from(&dir));
            }

            Ok(())
        })
        .manage(AppState {
            index: Mutex::new(Index::empty()),
            cfg: Mutex::new(Config::default()),
        })
        .invoke_handler(tauri::generate_handler![
            get_config,
            search,
            rescan,
            pick_folder,
            open_file,
            reveal_file,
            hide_window,
            quit_app
        ])
        .on_window_event(|window, event| match event {
            WindowEvent::CloseRequested { api, .. } => {
                // 点关闭 = 隐藏到托盘
                api.prevent_close();
                let _ = window.hide();
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
