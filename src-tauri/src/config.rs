//! 应用配置：知识库根（结构化）+ 范围规则 + 窗口形态 + 快捷键
//! 存于系统应用配置目录下的 config.json
//! 数据模型在 scope.rs（无 tauri 依赖，独立工具可挂载）

pub use crate::scope::KnowledgeRoot;
use crate::scope::{basename_of, ScopeConfig};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;
use tauri::Manager;

pub const DEFAULT_SHORTCUT: &str = "Alt+Space";
pub const DEFAULT_WIDTH: f64 = 600.0;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    /// v0.3 兼容字段：等价于 knowledge_dirs 单元素；仅作迁移来源保留
    #[serde(default)]
    pub knowledge_dir: Option<String>,
    /// v0.4 兼容字段：迁移来源保留
    #[serde(default)]
    pub knowledge_dirs: Vec<String>,
    /// v0.5 规范字段：结构化知识库根
    #[serde(default)]
    pub roots: Vec<KnowledgeRoot>,
    /// 搜寻范围规则
    #[serde(default)]
    pub scope: ScopeConfig,
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
    /// 搜索历史（最近 20 条，最新在前）
    #[serde(default)]
    pub search_history: Vec<String>,
}

impl Config {
    /// 生效的知识库根（enabled=true）
    pub fn effective_roots(&self) -> Vec<KnowledgeRoot> {
        self.roots.iter().filter(|r| r.enabled).cloned().collect()
    }

    /// 同步用 (路径, 显示名) 对
    pub fn root_pairs(&self) -> Vec<(PathBuf, String)> {
        self.effective_roots()
            .into_iter()
            .map(|r| {
                let label = if r.label.is_empty() {
                    basename_of(&r.path)
                } else {
                    r.label.clone()
                };
                (PathBuf::from(r.path), label)
            })
            .collect()
    }
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
    let mut cfg: Config = fs::read_to_string(path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default();
    migrate(&mut cfg);
    cfg
}

/// 旧版字段迁移：roots 为空时由 knowledge_dirs / knowledge_dir 合成
fn migrate(cfg: &mut Config) {
    if !cfg.roots.is_empty() {
        return;
    }
    let mut seen: Vec<String> = Vec::new();
    for d in cfg.knowledge_dirs.iter().chain(cfg.knowledge_dir.iter()) {
        let d = d.trim();
        if d.is_empty() || seen.contains(&d.to_string()) {
            continue;
        }
        seen.push(d.to_string());
        cfg.roots.push(KnowledgeRoot {
            path: d.to_string(),
            label: basename_of(d),
            enabled: true,
        });
    }
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
            knowledge_dirs: Vec::new(),
            roots: Vec::new(),
            scope: ScopeConfig::default(),
            x: None,
            y: None,
            width: DEFAULT_WIDTH,
            pin_mode: false,
            shortcut: DEFAULT_SHORTCUT.to_string(),
            search_history: Vec::new(),
        }
    }
}
