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
pub const DEFAULT_SELECTION_SHORTCUT: &str = "Alt+Shift+Space";
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
    pub last_sync: Option<u64>,
    /// 最近一次同步的错误信息（成功后清空）
    #[serde(default)]
    pub last_error: Option<String>,
}

fn default_git_enabled() -> bool {
    true
}

/// WebDAV 远程目录源：镜像到本地目录后注册为知识根（与 git 源同模式）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WebdavSource {
    /// 显示名（同时是镜像目录名与 @前缀标签）
    pub name: String,
    /// 集合 URL（如 https://dav.jianguoyun.com/dav/notes/）
    pub url: String,
    #[serde(default)]
    pub username: String,
    #[serde(default)]
    pub password: String,
    #[serde(default = "default_git_enabled")]
    pub enabled: bool,
    /// 自动同步间隔（分钟），0 = 仅手动
    #[serde(default)]
    pub interval_min: u64,
    #[serde(default)]
    pub last_sync: Option<u64>,
    #[serde(default)]
    pub last_error: Option<String>,
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
    /// v0.8.42：可选划词搜索，默认关闭
    #[serde(default)]
    pub selection_search_enabled: bool,
    /// 划词搜索快捷键，独立于窗口显隐快捷键
    #[serde(default = "default_selection_shortcut")]
    pub selection_shortcut: String,
    /// 显式允许 Ctrl+C 回退；默认关闭，仅快照/恢复纯文本格式
    #[serde(default)]
    pub selection_clipboard_fallback: bool,
    /// 搜索历史（最近 20 条，最新在前）
    #[serde(default)]
    pub search_history: Vec<String>,
    /// 范围预设列表
    #[serde(default)]
    pub presets: Vec<ScopePreset>,
    /// Git 仓库同步源
    #[serde(default)]
    pub git_sources: Vec<GitRepoSource>,
    /// WebDAV 远程目录源
    #[serde(default)]
    pub webdav_sources: Vec<WebdavSource>,
    /// 各范围使用次数（用于排序和统计）
    #[serde(default)]
    pub scope_usage: HashMap<String, u64>,
    /// AI 引擎（OpenAI 兼容接口；默认关闭，未启用时全部静默回退 BM25）
    #[serde(default)]
    pub ai: AiConfig,
    /// 本地 HTTP 检索接口（供其它程序调用；仅绑定 127.0.0.1）
    #[serde(default = "default_api")]
    pub api: ApiConfig,
    /// 展开态窗口高度（可拖拽调整；Resized 事件自动记忆）
    #[serde(default = "default_expanded_h")]
    pub expanded_h: f64,
}

/// 本地 HTTP 接口配置（改端口/开关后需重启应用生效）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApiConfig {
    /// 默认启用（仅监听 127.0.0.1，不暴露局域网）
    #[serde(default = "default_api_enabled")]
    pub enabled: bool,
    #[serde(default = "default_api_port")]
    pub port: u16,
    /// 访问令牌；空 = 不鉴权（仅本机可达）
    #[serde(default)]
    pub token: String,
}

fn default_api_enabled() -> bool { true }
fn default_api_port() -> u16 { 18230 }
fn default_expanded_h() -> f64 { 600.0 }
fn default_api() -> ApiConfig {
    ApiConfig { enabled: default_api_enabled(), port: default_api_port(), token: String::new() }
}

/// AI 引擎配置（Phase 1：结果筛选/重排）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AiConfig {
    /// 是否启用（关闭时前端不显示 AI 入口，后端拒绝调用）
    #[serde(default)]
    pub enabled: bool,
    /// OpenAI 兼容 base_url，如 https://api.example.com/v1 或 http://127.0.0.1:11434/v1
    #[serde(default)]
    pub base_url: String,
    /// API Key（明文或 dpapi: 密文；本地模型可留空）
    #[serde(default)]
    pub api_key: String,
    /// 模型名
    #[serde(default)]
    pub model: String,
    /// 单次调用超时（毫秒）
    #[serde(default = "default_ai_timeout_ms")]
    pub timeout_ms: u64,
    /// 送入模型筛选的候选段数
    #[serde(default = "default_ai_top_n")]
    pub top_n: usize,
}

fn default_ai_timeout_ms() -> u64 { 20_000 }
fn default_ai_top_n() -> usize { 20 }

impl Default for AiConfig {
    fn default() -> Self {
        AiConfig {
            enabled: false,
            base_url: String::new(),
            api_key: String::new(),
            model: String::new(),
            timeout_ms: default_ai_timeout_ms(),
            top_n: default_ai_top_n(),
        }
    }
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

    /// 校验所有会参与本地目录/元数据路径构造的同步源名。
    pub fn validate(&self) -> Result<(), String> {
        crate::safe_path::validate_source_names(
            self.git_sources.iter().map(|source| source.name.as_str()),
        )?;
        crate::safe_path::validate_source_names(
            self.webdav_sources.iter().map(|source| source.name.as_str()),
        )?;
        Ok(())
    }

    /// 配置 JSON 的统一反序列化入口：解析后立即做安全校验。
    pub fn from_json(json: &str) -> Result<Config, String> {
        let cfg: Config = serde_json::from_str(json).map_err(|e| format!("配置格式错误: {e}"))?;
        cfg.validate()?;
        Ok(cfg)
    }
}

fn default_width() -> f64 {
    DEFAULT_WIDTH
}
fn default_shortcut() -> String {
    DEFAULT_SHORTCUT.to_string()
}
fn default_selection_shortcut() -> String {
    DEFAULT_SELECTION_SHORTCUT.to_string()
}

fn config_path(app: &tauri::AppHandle) -> Option<PathBuf> {
    let dir = app.path().app_config_dir().ok()?;
    Some(dir.join("config.json"))
}

pub fn load(app: &tauri::AppHandle) -> Config {
    let Some(path) = config_path(app) else {
        return Config::default();
    };
    let mut cfg: Config = match fs::read_to_string(path) {
        Ok(s) => match Config::from_json(&s) {
            Ok(c) => c,
            // 解析或安全校验失败不静默：保留原文件，打日志后回退默认配置。
            Err(e) => {
                crate::kdlog!(warn, "config", "config.json 解析/校验失败，回退默认配置: {e}");
                Config::default()
            }
        },
        Err(_) => Config::default(),
    };
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
    if let Err(e) = cfg.validate() {
        crate::kdlog!(error, "config", "拒绝写入非法配置: {e}");
        return;
    }
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
            selection_search_enabled: false,
            selection_shortcut: DEFAULT_SELECTION_SHORTCUT.to_string(),
            selection_clipboard_fallback: false,
            search_history: Vec::new(),
            presets: Vec::new(),
            git_sources: Vec::new(),
            webdav_sources: Vec::new(),
            scope_usage: HashMap::new(),
            ai: AiConfig::default(),
            api: ApiConfig { enabled: true, port: 18230, token: String::new() },
            expanded_h: 600.0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legacy_config_defaults_selection_search_off() {
        let cfg: Config = serde_json::from_str("{}").expect("旧配置应可解析");
        assert!(!cfg.selection_search_enabled);
        assert!(!cfg.selection_clipboard_fallback);
        assert_eq!(cfg.selection_shortcut, DEFAULT_SELECTION_SHORTCUT);
    }

    #[test]
    fn selection_fields_round_trip() {
        let mut cfg = Config::default();
        cfg.selection_search_enabled = true;
        cfg.selection_shortcut = "Ctrl+Alt+S".into();
        cfg.selection_clipboard_fallback = true;
        let json = serde_json::to_string(&cfg).expect("序列化");
        let loaded: Config = serde_json::from_str(&json).expect("反序列化");
        assert!(loaded.selection_search_enabled);
        assert_eq!(loaded.selection_shortcut, "Ctrl+Alt+S");
        assert!(loaded.selection_clipboard_fallback);
    }

    #[test]
    fn path_security_config_accepts_legacy_and_safe_sources() {
        assert!(Config::from_json("{}").is_ok());
        let cfg = Config::from_json(
            r#"{
                "selection_search_enabled": false,
                "selection_shortcut": "Alt+Shift+Space",
                "git_sources": [{"name":"repo-1","url":"https://example.com/repo.git"}],
                "webdav_sources": [{"name":"中文 源","url":"https://example.com/dav"}]
            }"#,
        )
        .unwrap();
        assert!(!cfg.selection_search_enabled);
        assert_eq!(cfg.git_sources[0].name, "repo-1");
        assert_eq!(cfg.webdav_sources[0].name, "中文 源");
    }

    #[test]
    fn path_security_config_rejects_unsafe_and_case_colliding_sources() {
        assert!(Config::from_json(
            r#"{"git_sources":[{"name":"../evil","url":"https://example.com/repo.git"}]}"#
        )
        .is_err());
        assert!(Config::from_json(
            r#"{"webdav_sources":[{"name":"..\\evil","url":"https://example.com/dav"}]}"#
        )
        .is_err());
        assert!(Config::from_json(
            r#"{"git_sources":[
                {"name":"Repo","url":"https://example.com/a.git"},
                {"name":"repo","url":"https://example.com/b.git"}
            ]}"#
        )
        .is_err());
        assert!(Config::from_json(
            r#"{"webdav_sources":[
                {"name":"Σ","url":"https://example.com/a"},
                {"name":"ς","url":"https://example.com/b"}
            ]}"#
        )
        .is_err());
    }
}
