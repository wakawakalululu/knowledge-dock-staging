//! 应用配置：知识库根（结构化）+ 范围规则 + 窗口形态 + 快捷键
//! 存于系统应用配置目录下的 config.json
//! 数据模型在 scope.rs（无 tauri 依赖，独立工具可挂载）

pub use crate::scope::KnowledgeRoot;
use crate::scope::{basename_of, ScopeConfig};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;
use tauri::Manager;

pub const DEFAULT_SHORTCUT: &str = "Alt+Space";
pub const DEFAULT_WIDTH: f64 = 600.0;

/// 范围预设：保存一组知识库根+范围规则的命名快照
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScopePreset {
    pub name: String,
    pub roots: Vec<KnowledgeRoot>,
    pub exclude_globs: Vec<String>,
    pub include_exts: Vec<String>,
    /// v0.8.1：白名单规则（旧预设缺省为空 = 全收）
    #[serde(default)]
    pub include_globs: Vec<String>,
    /// v0.8.1：时间窗天数（旧预设缺省 0 = 不限）
    #[serde(default)]
    pub max_age_days: u64,
}

/// Git 仓库同步源：最小克隆到本地目录后注册为知识根，参与统一检索；
/// interval_min>0 时由后台线程定时拉取更新（0 = 仅手动）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GitRepoSource {
    /// 显示名（同时是克隆目录名与 @前缀标签）
    pub name: String,
    pub url: String,
    /// 分支；空 = 远端默认分支（克隆成功后回填实际分支）
    #[serde(default)]
    pub branch: String,
    #[serde(default = "default_git_enabled")]
    pub enabled: bool,
    /// 自动更新间隔（分钟）
    #[serde(default)]
    pub interval_min: u64,
    /// 最近一次成功同步的 unix 秒
    #[serde(default)]
    pub last_sync: u64,
    /// 最近一次同步的错误信息（成功后清空）
    #[serde(default)]
    pub last_error: Option<String>,
}

fn default_git_enabled() -> bool {
    true
}

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
    /// 范围预设列表
    #[serde(default)]
    pub presets: Vec<ScopePreset>,
    /// Git 仓库同步源
    #[serde(default)]
    pub git_sources: Vec<GitRepoSource>,
    /// 各范围使用次数（用于排序和统计）
    #[serde(default)]
    pub scope_usage: HashMap<String, u64>,
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

    /// 同步用单根规格（含每库独立范围覆盖）
    pub fn root_specs(&self) -> Vec<crate::scope::RootSpec> {
        self.effective_roots()
            .into_iter()
            .map(|r| {
                let label = if r.label.is_empty() {
                    basename_of(&r.path)
                } else {
                    r.label.clone()
                };
                crate::scope::RootSpec {
                    path: PathBuf::from(r.path),
                    label,
                    excludes: r.root_exclude_globs.clone(),
                    exts: r.root_exts.clone(),
                    max_file_kb: r.root_max_file_kb,
                }
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
    // 容错校验：无效值回退默认
    if cfg.width < 320.0 || cfg.width > 1600.0 { cfg.width = 600.0; }
    if cfg.roots.iter().any(|r| r.path.is_empty()) {
        cfg.roots.retain(|r| !r.path.is_empty());
    }
    if cfg.scope.max_file_kb == 0 { cfg.scope.max_file_kb = 4096; }
    if cfg.scope.include_exts.is_empty() { cfg.scope.include_exts = vec!["md".into(), "markdown".into(), "txt".into()]; }
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
            root_exclude_globs: Vec::new(),
            root_exts: Vec::new(),
            root_max_file_kb: 0,
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
            presets: Vec::new(),
            git_sources: Vec::new(),
            scope_usage: HashMap::new(),
        }
    }
}
