//! Git 仓库同步 —— 通过系统 git CLI 完成最小克隆与浅更新（零新增 Rust 依赖）
//!
//! 克隆：git clone --depth 1 --single-branch [--branch B] <url> <dir>
//! 更新：git fetch --depth 1 origin <branch> && git reset --hard FETCH_HEAD
//!       && git clean -fd（对远端 force push 也稳健）
//! 安全：GIT_TERMINAL_PROMPT=0 禁终端提示挂起（GUI 凭证助手仍可用）；
//!       http.lowSpeedLimit 兜底断流；仓库名 sanitize 防路径穿越。

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
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
    run(Some(&dir), &["rev-parse", "--abbrev-ref", "HEAD"])
}

static REBRANCH_TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

/// 生成与合法同步源名空间不冲突的临时目录名。名称中的 `..` 不会被
/// safe_source_component 接受，因此不可能与用户来源同名；仅用于 Windows 合法文件名。
fn rebranch_temp_path(dir: &Path) -> Result<PathBuf, String> {
    let parent = dir
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .ok_or_else(|| format!("仓库目录缺少父目录: {}", dir.display()))?;
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or_default();
    for _ in 0..128 {
        let serial = REBRANCH_TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
        let name = format!(".kd..rebranch-{}-{nanos}-{serial}", std::process::id());
        let candidate = crate::safe_path::safe_join_under(parent, &name)?;
        match std::fs::symlink_metadata(&candidate) {
            Ok(_) => continue,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(candidate),
            Err(error) => {
                return Err(format!("检查分支临时目录失败 {}: {error}", candidate.display()))
            }
        }
    }
    Err("无法分配唯一的分支临时目录".into())
}

fn cleanup_rebranch_temp(path: &Path) {
    let Some(parent) = path.parent() else { return };
    let Some(name) = path.file_name().and_then(|name| name.to_str()) else { return };
    if let Ok(safe_path) = crate::safe_path::safe_join_under(parent, name) {
        let _ = std::fs::remove_dir_all(safe_path);
    }
}

/// 浅更新到远端最新（fetch --depth 1 + reset --hard + clean -fd）
/// 分支失配（单分支浅克隆无法直接 checkout 远端其它分支）→ 先克隆新分支到临时目录，成功后才替换旧克隆
pub fn pull_repo(dir: &Path, url: &str, branch: &str) -> Result<String, String> {
    let parent = dir
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .ok_or_else(|| format!("仓库目录缺少父目录: {}", dir.display()))?;
    let source_name = dir
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| format!("仓库目录名无效: {}", dir.display()))?;
    crate::safe_path::safe_source_component(source_name)?;
    let dir = crate::safe_path::safe_join_under(parent, source_name)?;
    if !dir.join(".git").exists() {
        return Err(format!("目录不是 git 仓库: {}", dir.display()));
    }
    let branch = branch.trim();
    if branch.is_empty() {
        return Err("分支名为空".into());
    }
    let cur = current_branch(&dir).unwrap_or_default();
    if cur != branch {
        let tmp = rebranch_temp_path(&dir)?;
        if let Err(error) = clone_repo(url, branch, &tmp) {
            cleanup_rebranch_temp(&tmp);
            return Err(format!("克隆分支 {branch} 失败: {error}"));
        }
        // 网络克隆期间目录可能被替换； destructive 操作前再次检查根/祖先与目标。
        let tmp_name = tmp
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or_else(|| "分支临时目录名无效".to_string())?;
        let tmp = match crate::safe_path::safe_join_under(parent, tmp_name) {
            Ok(safe_tmp) => safe_tmp,
            Err(error) => {
                cleanup_rebranch_temp(&tmp);
                return Err(error);
            }
        };
        let dir = match crate::safe_path::safe_join_under(parent, source_name) {
            Ok(safe_dir) => safe_dir,
            Err(error) => {
                cleanup_rebranch_temp(&tmp);
                return Err(error);
            }
        };
        if let Err(error) = std::fs::remove_dir_all(&dir) {
            cleanup_rebranch_temp(&tmp);
            return Err(format!("删除旧克隆失败: {error}"));
        }
        let tmp = crate::safe_path::safe_join_under(parent, tmp_name)?;
        if let Err(error) = std::fs::rename(&tmp, &dir) {
            // 旧目录已删除，tmp 是唯一的新克隆；失败时必须保留并报告路径，
            // 不能再执行清理而把用户数据一并删除。
            return Err(format!(
                "替换克隆目录失败: {error}；新克隆保留在 {}",
                tmp.display()
            ));
        }
        let short = run(Some(&dir), &["rev-parse", "--short", "HEAD"])?;
        return Ok(format!("已切换分支 {branch}@{short}（重建克隆）"));
    }
    run(Some(&dir), &["fetch", "--depth", "1", "origin", branch])?;
    // fetch 结束后、reset/clean 等破坏性命令前复核目录边界。
    let dir = crate::safe_path::safe_join_under(parent, source_name)?;
    run(Some(&dir), &["reset", "--hard", "FETCH_HEAD"])?;
    run(Some(&dir), &["clean", "-fd"])?;
    let short = run(Some(&dir), &["rev-parse", "--short", "HEAD"])?;
    Ok(format!("已同步 {branch}@{short}"))
}

/// 删除克隆目录：名称与最终路径都必须处于 repos 根内。
pub fn remove_repo_dir(repos_root: &Path, name: &str) -> Result<(), String> {
    crate::safe_path::safe_source_component(name)?;
    let dir = crate::safe_path::safe_join_under(repos_root, name)?;
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

    #[test]
    fn path_security_rebranch_temp_avoids_source_namespace() {
        let base = std::env::temp_dir().join(format!(
            "kd-git-rebranch-path-security-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&base);
        let root = base.join("repos");
        let existing = root.join("x.rebranch-tmp");
        std::fs::create_dir_all(&existing).unwrap();
        std::fs::write(existing.join("sentinel.txt"), "keep").unwrap();

        let first = rebranch_temp_path(&root.join("x")).unwrap();
        let second = rebranch_temp_path(&root.join("x")).unwrap();
        assert_ne!(first, second);
        assert_ne!(first, existing);
        assert_ne!(second, existing);
        let temp_name = first.file_name().unwrap().to_str().unwrap();
        assert!(crate::safe_path::safe_source_component(temp_name).is_err());
        assert_eq!(
            std::fs::read_to_string(existing.join("sentinel.txt")).unwrap(),
            "keep"
        );
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn path_security_remove_repo_rejects_escape() {
        let base = std::env::temp_dir().join(format!(
            "kd-git-path-security-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&base);
        let root = base.join("repos");
        let outside = base.join("outside");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(outside.join("sentinel.txt"), "keep").unwrap();

        assert!(remove_repo_dir(&root, "../outside").is_err());
        assert!(remove_repo_dir(&root, "..\\outside").is_err());
        assert_eq!(
            std::fs::read_to_string(outside.join("sentinel.txt")).unwrap(),
            "keep"
        );
        let _ = std::fs::remove_dir_all(&base);
    }
}
