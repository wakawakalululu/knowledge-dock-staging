//! 应用配置：知识库目录 + 窗口形态 + 快捷键，存于系统应用配置目录下的 config.json

use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;
use tauri::Manager;

pub const DEFAULT_SHORTCUT: &str = "Alt+Space";
pub const DEFAULT_WIDTH: f64 = 600.0;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    /// 知识库根目录（绝对路径）
    #[serde(default)]
    pub knowledge_dir: Option<String>,
    /// 窗口位置（物理像素），用于重启恢复
    #[serde(default)]
    pub x: Option<i32>,
    #[serde(default)]
    pub y: Option<i32>,
    /// 悬浮窗宽度（逻辑像素）
    #[serde(default = "default_width")]
    pub width: f64,
    /// 钉住模式：始终显示、禁用拖动、Esc 不隐藏
    #[serde(default)]
    pub pin_mode: bool,
    /// 全局快捷键（如 "Alt+Space"）
    #[serde(default = "default_shortcut")]
    pub shortcut: String,
}

fn default_width() -> f64 {
    DEFAULT_WIDTH
}
fn default_shortcut() -> String {
    DEFAULT_SHORTCUT.to_string()
}

fn config_path(app: &tauri::AppHandle) -> Option<PathBuf> {
    let dir = app.path().app_config_dir().ok()?;
    Some(dir.join("config.json"))
}

pub fn load(app: &tauri::AppHandle) -> Config {
    let Some(path) = config_path(app) else {
        return Config::default();
    };
    fs::read_to_string(path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

pub fn save(app: &tauri::AppHandle, cfg: &Config) {
    let Some(path) = config_path(app) else { return };
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    if let Ok(json) = serde_json::to_string_pretty(cfg) {
        let _ = fs::write(path, json);
    }
}

impl Default for Config {
    fn default() -> Self {
        Config {
            knowledge_dir: None,
            x: None,
            y: None,
            width: DEFAULT_WIDTH,
            pin_mode: false,
            shortcut: DEFAULT_SHORTCUT.to_string(),
        }
    }
}
