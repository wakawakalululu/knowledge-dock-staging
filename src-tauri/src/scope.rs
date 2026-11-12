//! 范围相关纯数据模型（无 tauri 依赖，可被独立工具挂载）：
//! KnowledgeRoot（知识库根）与 ScopeConfig（搜寻范围规则）

use serde::{Deserialize, Serialize};

/// 一个知识库根：路径 + 显示名 + 启用开关（可临时停用而不删除）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KnowledgeRoot {
    pub path: String,
    /// 显示名（面包屑/根筛选 chips 用）；空 = 取目录名
    #[serde(default)]
    pub label: String,
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// 每库独立排除规则（与全局规则合并）
    #[serde(default)]
    pub root_exclude_globs: Vec<String>,
    /// 每库独立扩展名覆盖（空 = 用全局 include_exts）
    #[serde(default)]
    pub root_exts: Vec<String>,
    /// 每库单文件大小上限覆盖 KB（0 = 用全局 max_file_kb）
    #[serde(default)]
    pub root_max_file_kb: u64,
}

fn default_true() -> bool {
    true
}

/// 同步用的单根规格：路径 + 显示名 + 每库独立范围（排除/扩展名/大小上限）
#[derive(Debug, Clone)]
pub struct RootSpec {
    pub path: std::path::PathBuf,
    pub label: String,
    pub excludes: Vec<String>,
    pub exts: Vec<String>,
    pub max_file_kb: u64,
}

/// 搜寻范围规则
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScopeConfig {
    /// 排除规则（相对知识库根的路径）：
    /// - `dir/`  → 排除该目录下所有文件
    /// - `*.tmp.md` → 通配文件名
    /// - `a/b.md` → 精确相对路径
    #[serde(default = "default_excludes")]
    pub exclude_globs: Vec<String>,
    /// 包含白名单（相对知识库根的路径，语法同排除规则）：
    /// 空 = 全收；非空 = 仅收录命中的文件/目录（先白名单后排除）
    #[serde(default)]
    pub include_globs: Vec<String>,
    /// 收录的扩展名（小写、不含点）
    #[serde(default = "default_exts")]
    pub include_exts: Vec<String>,
    /// 单文件大小上限（KB）
    #[serde(default = "default_max_kb")]
    pub max_file_kb: u64,
    /// 时间窗：只索引最近 N 天内有修改的文件（0 = 不限）
    #[serde(default)]
    pub max_age_days: u64,
}

fn default_excludes() -> Vec<String> {
    Vec::new()
}
fn default_exts() -> Vec<String> {
    vec!["md".into(), "markdown".into(), "txt".into()]
}
fn default_max_kb() -> u64 {
    4096
}

impl Default for ScopeConfig {
    fn default() -> Self {
        ScopeConfig {
            exclude_globs: default_excludes(),
            include_globs: Vec::new(),
            include_exts: default_exts(),
            max_file_kb: default_max_kb(),
            max_age_days: 0,
        }
    }
}

pub fn basename_of(path: &str) -> String {
    std::path::Path::new(path)
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| path.to_string())
}
