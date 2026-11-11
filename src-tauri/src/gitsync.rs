//! Git 仓库同步 —— 通过系统 git CLI 完成最小克隆与浅更新（零新增 Rust 依赖）
//!
//! 克隆：git clone --depth 1 --single-branch [--branch B] <url> <dir>
//! 更新：git fetch --depth 1 origin <branch> && git reset --hard FETCH_HEAD
//!       && git clean -fd（对远端 force push 也稳健）
//! 安全：GIT_TERMINAL_PROMPT=0 禁终端提示挂起（GUI 凭证助手仍可用）；
//!       http.lowSpeedLimit 兜底断流；仓库名 sanitize 防路径穿越。

use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

/// 当前 unix 秒
pub fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// 合法仓库名：字母/数字/-/_，长度 1..=64（防路径穿越与命令注入）
/// 含路径分隔符或 ".." 的输入直接拒绝，不做静默转换
pub fn sanitize_name(name: &str) -> Option<String> {
    let name = name.trim();
    if name.is_empty() || name.len() > 64 {
        return None;
    }
    if name.contains('/') || name.contains('\\') || name.contains("..") {
        return None;
    }
    let cleaned: String = name
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_' || *c == '.')
        .collect();
    // 剥离用户顺手带的 .git 后缀
    let cleaned = if cleaned.len() >= 4 && cleaned[..4].eq_ignore_ascii_case(".git") {
        cleaned[4..].to_string()
    } else if cleaned.len() >= 4 && cleaned.to_lowercase().ends_with(".git") {
        cleaned[..cleaned.len() - 4].to_string()
    } else {
        cleaned
    };
    let cleaned = cleaned.trim_matches('.').to_string();
    if cleaned.is_empty() {
        None
    } else {
        Some(cleaned)
    }
}

/// 从 URL 推导仓库名（最后一段，去 .git 后缀）
pub fn name_from_url(url: &str) -> String {
    let u = url.trim().trim_end_matches('/');
    let base = u
        .rsplit(['/', '\\', ':'])
        .find(|s| !s.is_empty())
        .unwrap_or("repo");
    base.trim_end_matches(".git").to_string()
}

/// 执行 git 命令；成功返回 stdout（trim），失败返回含 stderr 的错误
fn run(dir: Option<&Path>, args: &[&str]) -> Result<String, String> {
    let mut cmd = std::process::Command::new("git");
    // 断流兜底：60 秒内速度低于 1KB/s 即中止（仅对网络传输生效，本地操作忽略）
    cmd.arg("-c").arg("http.lowSpeedLimit=1000");
    cmd.arg("-c").arg("http.lowSpeedTime=60");
    // 禁终端凭证提示挂起；GUI 凭证管理器（如 Windows GCM）不受影响
    cmd.env("GIT_TERMINAL_PROMPT", "0");
    if let Some(d) = dir {
        cmd.current_dir(d);
    }
    cmd.args(args);
    let out = cmd
        .output()
        .map_err(|e| format!("无法执行 git（请确认已安装并加入 PATH）: {e}"))?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
    } else {
        Err(format!(
            "git {} 失败: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        ))
    }
}

/// 最小克隆（浅克隆单分支）；目录必须不存在
pub fn clone_repo(url: &str, branch: &str, dir: &Path) -> Result<(), String> {
    if dir.exists() {
        return Err(format!("本地目录已存在: {}", dir.display()));
    }
    if let Some(parent) = dir.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let mut args: Vec<&str> = vec!["clone", "--depth", "1", "--single-branch", "--no-tags"];
    let branch = branch.trim();
    let push_branch = branch.to_string();
    if !push_branch.is_empty() {
        args.push("--branch");
        args.push(&push_branch);
    }
    let url = url.trim();
    let dir_str = dir.to_string_lossy().to_string();
    args.push(url);
    args.push(&dir_str);
    run(None, &args).map(|_| ())
}

/// 读取当前分支名（克隆后回填用）
pub fn current_branch(dir: &Path) -> Result<String, String> {
    run(Some(dir), &["rev-parse", "--abbrev-ref", "HEAD"])
}

/// 浅更新到远端最新（fetch --depth 1 + reset --hard + clean -fd）
pub fn pull_repo(dir: &Path, branch: &str) -> Result<String, String> {
    if !dir.join(".git").exists() {
        return Err(format!("目录不是 git 仓库: {}", dir.display()));
    }
    let branch = branch.trim();
    if branch.is_empty() {
        return Err("分支名为空".into());
    }
    run(Some(dir), &["fetch", "--depth", "1", "origin", branch])?;
    run(Some(dir), &["reset", "--hard", "FETCH_HEAD"])?;
    run(Some(dir), &["clean", "-fd"])?;
    let short = run(Some(dir), &["rev-parse", "--short", "HEAD"])?;
    Ok(format!("已同步 {branch}@{short}"))
}

/// 删除克隆目录（name 必须已过 sanitize；双保险校验路径未逃逸根目录）
pub fn remove_repo_dir(repos_root: &Path, name: &str) -> Result<(), String> {
    let dir = repos_root.join(name);
    if !dir.starts_with(repos_root) {
        return Err("非法仓库名".into());
    }
    if dir.exists() {
        std::fs::remove_dir_all(&dir).map_err(|e| format!("删除目录失败: {e}"))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_rejects_traversal() {
        assert!(sanitize_name("../../etc").is_none());
        assert!(sanitize_name("a..b").is_none());
        assert!(sanitize_name("").is_none());
        assert_eq!(sanitize_name("my-repo_1.git"), Some("my-repo_1".into()));
    }

    #[test]
    fn name_from_url_works() {
        assert_eq!(name_from_url("https://github.com/u/proj.git"), "proj");
        assert_eq!(name_from_url("https://gitlab.com/u/proj/"), "proj");
    }
}
