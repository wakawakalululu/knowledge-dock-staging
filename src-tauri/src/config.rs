//! 应用配置：知识库目录 + 窗口位置，存于系统应用配置目录下的 config.json

use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;
use tauri::Manager;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Config {
    /// 知识库根目录（绝对路径）
    #[serde(default)]
    pub knowledge_dir: Option<String>,
    /// 窗口位置（物理像素），用于重启恢复
    #[serde(default)]
    pub x: Option<i32>,
    #[serde(default)]
    pub y: Option<i32>,
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
