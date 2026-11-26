//! 本地路径安全边界：远端相对路径、同步源名与目录拼接的统一校验。
//!
//! 这里只做确定性的词法与文件系统边界检查，不尝试解析 URL 或修改用户配置。
//! Windows 上尤其需要拒绝反斜杠、盘符、尾随点/空格和 reparse point，避免
//! `Path::join` 或文件系统归一化把看似相对的路径解释到目录之外。

use std::fs;
use std::path::{Component, Path, PathBuf};

/// 校验远端相对路径，返回可直接 join 到本地镜像根的 PathBuf。
///
/// 接受 `docs/a.md`、中文与空格文件名；拒绝空段、`.`、`..`、反斜杠、
/// 盘符/冒号、控制字符、绝对路径、UNC 和 Windows 非法文件名字符。
pub fn safe_relative_path(rel: &str) -> Result<PathBuf, String> {
    if rel.is_empty() {
        return Err("相对路径为空".into());
    }
    if rel.contains('\\') {
        return Err(format!("相对路径包含反斜杠: {rel}"));
    }
    if rel.contains(':') {
        return Err(format!("相对路径包含盘符或冒号: {rel}"));
    }
    if rel.chars().any(char::is_control) {
        return Err(format!("相对路径包含控制字符: {rel:?}"));
    }
    if rel.starts_with('/') || Path::new(rel).is_absolute() {
        return Err(format!("相对路径必须是相对路径: {rel}"));
    }

    for segment in rel.split('/') {
        if segment.is_empty() || segment == "." || segment == ".." {
            return Err(format!("相对路径包含非法段: {rel}"));
        }
        if segment.ends_with('.') || segment.ends_with(' ') {
            return Err(format!("文件名段不能以点或空格结尾: {segment}"));
        }
        if segment.chars().any(|c| matches!(c, '<' | '>' | '"' | '|' | '?' | '*')) {
            return Err(format!("文件名段包含 Windows 非法字符: {segment}"));
        }
        if is_reserved_windows_name(segment) {
            return Err(format!("文件名段使用 Windows 保留名: {segment}"));
        }
    }

    Ok(PathBuf::from(rel))
}

/// 校验同步源名：必须是单个安全 Windows 路径组件。
///
/// 保留安全的 Unicode 与内部空格旧名称，但不会 trim、重命名或静默过滤。
pub fn safe_source_component(name: &str) -> Result<(), String> {
    if name.is_empty() {
        return Err("同步源名为空".into());
    }
    if name.len() > 64 {
        return Err("同步源名超过 64 字节".into());
    }
    if name != name.trim() {
        return Err("同步源名不能有首尾空白".into());
    }
    if name == "." || name == ".." || name.contains("..") {
        return Err(format!("同步源名包含非法点段: {name}"));
    }
    if name.chars().any(|c| {
        c.is_control()
            || matches!(c, '/' | '\\' | ':' | '<' | '>' | '"' | '|' | '?' | '*')
    }) {
        return Err(format!("同步源名包含非法字符: {name:?}"));
    }
    if name.ends_with('.') || name.ends_with(' ') {
        return Err("同步源名不能以点或空格结尾".into());
    }
    if is_reserved_windows_name(name) {
        return Err(format!("同步源名使用 Windows 保留名: {name}"));
    }
    Ok(())
}

fn case_insensitive_key(value: &str) -> (String, String) {
    // 同时归一化大小写映射的两个方向，覆盖 Σ/ς、K/k 等 Unicode 简单
    // 大小写映射不一致；宁可拒绝可能冲突的旧名，也不静默合并目录。
    (value.to_lowercase(), value.to_uppercase())
}

fn case_keys_collide(a: &(String, String), b: &(String, String)) -> bool {
    a.0 == b.0 || a.1 == b.1 || a.0 == b.1 || a.1 == b.0
}

/// 判断两个路径字符串在 Windows 镜像上是否可能指向同一文件。
pub fn paths_case_collide(a: &str, b: &str) -> bool {
    case_keys_collide(&case_insensitive_key(a), &case_insensitive_key(b))
}

/// 校验一组来源名，并拒绝 Windows 上大小写不敏感的同名冲突。
pub fn validate_source_names<'a, I>(names: I) -> Result<(), String>
where
    I: IntoIterator<Item = &'a str>,
{
    let mut seen: Vec<(String, String)> = Vec::new();
    for name in names {
        safe_source_component(name)?;
        let key = case_insensitive_key(name);
        if seen.iter().any(|seen_key| case_keys_collide(seen_key, &key)) {
            return Err(format!("同步源名大小写冲突: {name}"));
        }
        seen.push(key);
    }
    Ok(())
}

/// 校验远端相对路径集合，拒绝在 Windows 镜像上会落到同一文件的大小写冲突。
pub fn validate_relative_paths_unique<'a, I>(paths: I) -> Result<(), String>
where
    I: IntoIterator<Item = &'a str>,
{
    let mut seen: Vec<(String, String)> = Vec::new();
    for rel in paths {
        safe_relative_path(rel)?;
        let key = case_insensitive_key(rel);
        if seen.iter().any(|seen_key| case_keys_collide(seen_key, &key)) {
            return Err(format!("WebDAV 相对路径大小写冲突: {rel}"));
        }
        seen.push(key);
    }
    Ok(())
}

/// 将相对路径安全地拼到 root 下，并拒绝已有 reparse point/junction 逃逸。
///
/// 词法检查先于文件系统检查；对已存在的目标做 canonicalize 边界复核，对
/// 尚不存在的目标检查其已有祖先。该函数不能消除极端的检查/使用竞态，但
/// 调用方必须在每一次实际写入或删除前重新调用。
pub fn safe_join_under(root: &Path, rel: &str) -> Result<PathBuf, String> {
    let relative = safe_relative_path(rel)?;
    let candidate = root.join(&relative);
    if !candidate.starts_with(root) {
        return Err(format!("路径逃逸出根目录: {}", candidate.display()));
    }

    // root 自身或其已有祖先是 junction 时，canonicalize(root) 也会把逃逸后的
    // 目录当作“根”，因此必须先拒绝整条已有 reparse point 链。
    reject_reparse_point_chain(root)?;

    let mut current = root.to_path_buf();
    for component in relative.components() {
        let Component::Normal(part) = component else {
            return Err(format!("相对路径包含非法组件: {rel}"));
        };
        current.push(part);
        match fs::symlink_metadata(&current) {
            Ok(meta) => {
                if is_reparse_point(&meta) {
                    return Err(format!("路径经过 reparse point/junction: {}", current.display()));
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => break,
            Err(e) => {
                return Err(format!("检查路径失败 {}: {e}", current.display()));
            }
        }
    }

    if candidate.exists() {
        let canonical_root = fs::canonicalize(root)
            .map_err(|e| format!("无法规范化根目录 {}: {e}", root.display()))?;
        let canonical_candidate = fs::canonicalize(&candidate)
            .map_err(|e| format!("无法规范化目标路径 {}: {e}", candidate.display()))?;
        if !canonical_candidate.starts_with(canonical_root) {
            return Err(format!("目标路径逃逸出根目录: {}", candidate.display()));
        }
    }

    Ok(candidate)
}

fn reject_reparse_point_chain(path: &Path) -> Result<(), String> {
    let mut current = path.to_path_buf();
    loop {
        match fs::symlink_metadata(&current) {
            Ok(meta) => {
                if is_reparse_point(&meta) {
                    return Err(format!("根路径经过 reparse point/junction: {}", current.display()));
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(format!("检查根路径失败 {}: {e}", current.display())),
        }
        if !current.pop() {
            break;
        }
    }
    Ok(())
}

fn is_reserved_windows_name(segment: &str) -> bool {
    // Windows 同时把 COM¹/²/³、LPT¹/²/³ 视为对应数字设备名。
    let normalized: String = segment
        .chars()
        .map(|character| match character {
            '¹' => '1',
            '²' => '2',
            '³' => '3',
            other => other,
        })
        .collect();
    let base = normalized
        .split('.')
        .next()
        .unwrap_or(&normalized)
        .trim_end_matches(' ')
        .to_ascii_uppercase();
    matches!(
        base.as_str(),
        "CON"
            | "PRN"
            | "AUX"
            | "NUL"
            | "COM1"
            | "COM2"
            | "COM3"
            | "COM4"
            | "COM5"
            | "COM6"
            | "COM7"
            | "COM8"
            | "COM9"
            | "LPT1"
            | "LPT2"
            | "LPT3"
            | "LPT4"
            | "LPT5"
            | "LPT6"
            | "LPT7"
            | "LPT8"
            | "LPT9"
    )
}

fn is_reparse_point(meta: &fs::Metadata) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        // FILE_ATTRIBUTE_REPARSE_POINT
        meta.file_attributes() & 0x0000_0400 != 0
    }
    #[cfg(not(windows))]
    {
        meta.file_type().is_symlink()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT_TEMP: AtomicU64 = AtomicU64::new(1);

    fn temp_dir(label: &str) -> PathBuf {
        let id = NEXT_TEMP.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "kd-path-security-{label}-{}-{id}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).expect("创建测试目录");
        path
    }

    #[test]
    fn path_security_relative_paths_accept_safe_names() {
        assert_eq!(safe_relative_path("a.md").unwrap(), PathBuf::from("a.md"));
        assert_eq!(
            safe_relative_path("docs/a b.md").unwrap(),
            PathBuf::from("docs/a b.md")
        );
        assert_eq!(
            safe_relative_path("中文/笔记.md").unwrap(),
            PathBuf::from("中文/笔记.md")
        );
    }

    #[test]
    fn path_security_relative_paths_reject_traversal_and_absolute() {
        for bad in [
            "",
            "../x",
            "..\\x",
            "dir/../x",
            "dir\\..\\x",
            "/abs",
            "\\abs",
            "C:/x",
            "C:\\x",
            "C:x",
            "//server/share/x",
            "dir//x",
            "dir/./x",
            "dir/\u{0}x",
            "COM¹.md",
            "LPT².txt",
            "COM³",
        ] {
            assert!(safe_relative_path(bad).is_err(), "不应接受 {bad:?}");
        }
    }

    #[test]
    fn path_security_source_names_validate_components_and_case_collisions() {
        for good in ["repo", "my-repo_1", "中文 源", "repo name"] {
            assert!(safe_source_component(good).is_ok(), "应接受 {good:?}");
        }
        for bad in [
            "", ".", "..", "a..b", "../x", "a/b", "a\\b", "C:x", " repo",
            "repo ", "repo.", "NUL", "con.txt", "bad?name", "bad*name", "COM¹",
            "LPT².txt", "COM³",
        ] {
            assert!(safe_source_component(bad).is_err(), "不应接受 {bad:?}");
        }
        assert!(validate_source_names(["Repo", "other"]).is_ok());
        assert!(validate_source_names(["Repo", "repo"]).is_err());
        assert!(validate_source_names(["Σ", "ς"]).is_err());
        assert!(validate_source_names(["K", "k"]).is_err());
    }

    #[test]
    fn path_security_relative_paths_reject_case_collisions() {
        assert!(validate_relative_paths_unique(["docs/ok.md", "docs/Other.md"]).is_ok());
        assert!(validate_relative_paths_unique(["docs/ok.md", "docs/OK.md"]).is_err());
        assert!(validate_relative_paths_unique(["Σ.md", "ς.md"]).is_err());
        assert!(validate_relative_paths_unique(["../x"]).is_err());
    }

    #[test]
    fn path_security_join_rejects_existing_junction() {
        let base = temp_dir("junction");
        let root = base.join("root");
        let outside = base.join("outside");
        fs::create_dir_all(&root).unwrap();
        fs::create_dir_all(&outside).unwrap();
        fs::write(outside.join("sentinel.txt"), "keep").unwrap();

        let link = root.join("link");
        let status = std::process::Command::new("cmd")
            .args(["/C", "mklink", "/J"])
            .arg(&link)
            .arg(&outside)
            .output()
            .expect("创建 junction");
        assert!(status.status.success(), "junction 创建失败");

        assert!(safe_join_under(&root, "link/sentinel.txt").is_err());
        assert!(safe_join_under(&root, "link/new.txt").is_err());
        assert_eq!(
            fs::read_to_string(outside.join("sentinel.txt")).unwrap(),
            "keep"
        );

        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn path_security_join_rejects_root_and_ancestor_junction() {
        let base = temp_dir("root-junction");
        let outside = base.join("outside");
        fs::create_dir_all(&outside).unwrap();
        fs::write(outside.join("sentinel.txt"), "keep").unwrap();

        let root_link = base.join("root-link");
        let status = std::process::Command::new("cmd")
            .args(["/C", "mklink", "/J"])
            .arg(&root_link)
            .arg(&outside)
            .output()
            .expect("创建根 junction");
        assert!(status.status.success(), "根 junction 创建失败");

        assert!(safe_join_under(&root_link, "new.txt").is_err());
        assert!(safe_join_under(&root_link, "sentinel.txt").is_err());

        let nested_root = root_link.join("nested");
        fs::create_dir_all(&nested_root).unwrap();
        assert!(safe_join_under(&nested_root, "new.txt").is_err());
        assert_eq!(
            fs::read_to_string(outside.join("sentinel.txt")).unwrap(),
            "keep"
        );
        assert!(!outside.join("new.txt").exists());

        let _ = fs::remove_dir_all(&base);
    }
}
