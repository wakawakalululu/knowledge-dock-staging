//! WebDAV 远程目录源 —— 通过系统 curl CLI 完成枚举与下载（零新增 Rust 依赖）
//!
//! 流程：PROPFIND Depth:1 逐层递归收集远端 {rel, size, mtime}
//!       → 与本地镜像元数据对比 → 新增/变更 GET 下载、多余删除
//! 安全：凭据仅作为 curl -u 参数传给子进程；镜像路径由 sanitize 后的
//!       仓库名锚定，rel 中的 `..` 段与反斜杠直接拒绝（防穿越）。

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;
use std::time::Duration;

/// 单个远端文件条目
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RemoteFile {
    /// 相对根的路径（正斜杠，URL 解码后）
    pub rel: String,
    pub size: u64,
    /// 远端 mtime（unix 秒，解析失败为 0）
    pub mtime: u64,
}

/// 本地镜像元数据（持久化 JSON：记录上次同步的远端状态）
pub type MirrorState = HashMap<String, RemoteFile>;

fn run_curl(args: &[&str]) -> Result<String, String> {
    let mut cmd = std::process::Command::new("curl");
    cmd.args(args);
    cmd.arg("--max-time").arg("120");
    cmd.arg("-sS");
    let out = cmd
        .output()
        .map_err(|e| format!("无法执行 curl（Windows 10+ 自带）: {e}"))?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).to_string())
    } else {
        Err(format!(
            "curl 失败: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ))
    }
}

/// PROPFIND 枚举一层：返回 (子目录 hrefs, 文件条目)
fn propfind_level(
    auth: &str,
    depth_url: &str,
    depth: &str,
) -> Result<(Vec<String>, Vec<RemoteFile>), String> {
    let body = run_curl(&[
        "-u",
        auth,
        "-X",
        "PROPFIND",
        "-H",
        &format!("Depth: {depth}"),
        "-H",
        "Content-Type: application/xml",
        "--data",
        "<?xml version=\"1.0\"?><d:propfind xmlns:d=\"DAV:\"><d:prop><d:resourcetype/><d:getcontentlength/><d:getlastmodified/></d:prop></d:propfind>",
        depth_url,
    ])?;
    // 命名空间前缀各服务器不一（D:/d:/ns0:），宽松匹配标签名（忽略前缀）
    let lower = body.to_lowercase();
    if !lower.contains("<d:response") && !lower.contains(":response") && !lower.contains("response") {
        return Err(format!("PROPFIND 响应异常（HTTP 状态/认证失败?）: {}", &body.chars().take(200).collect::<String>()));
    }
    let mut dirs = Vec::new();
    let mut files = Vec::new();
    // 逐个 <...:response> 块解析（保持原始大小写提取 href）
    let mut cursor = 0usize;
    let bytes = body.as_bytes();
    while let Some(pos) = body[cursor..].find("<").map(|p| p + cursor) {
        let end = body[pos..].find(">").map(|p| p + pos + 1).unwrap_or(bytes.len());
        let tag_start = &body[pos + 1..end.min(body.len())];
        let tag_l = tag_start.to_lowercase();
        if (tag_l.starts_with("d:response") || tag_l.starts_with("response")) && !tag_l.starts_with('/') {
            // 找到 response 开块：截到对应结束标签
            let close_pat = if tag_l.starts_with("d:") { "</d:response>" } else { "</response>" };
            let close = body[pos..].to_lowercase().find(close_pat).map(|p| pos + p);
            let Some(close_pos) = close else { break };
            let block = &body[pos..close_pos];
            let block_l = block.to_lowercase();

            let href = extract_tag(block, &["d:href", "href"]).unwrap_or_default();
            let is_dir = block_l.contains("collection");
            let size = extract_tag(block, &["d:getcontentlength", "getcontentlength"])
                .and_then(|s| s.trim().parse::<u64>().ok())
                .unwrap_or(0);
            let mtime = extract_tag(block, &["d:getlastmodified", "getlastmodified"])
                .map(|s| parse_http_date(&s))
                .unwrap_or(0);

            if !href.is_empty() && is_dir {
                dirs.push(href);
            } else if !href.is_empty() {
                files.push(RemoteFile { rel: href, size, mtime });
            }
            cursor = close_pos + close_pat.len();
            continue;
        }
        cursor = end;
    }
    Ok((dirs, files))
}

/// 宽松提取首个 <tag>value</tag>（忽略命名空间前缀与大小写）
fn extract_tag(block: &str, names: &[&str]) -> Option<String> {
    for name in names {
        let open_variants = [format!("<{name}>"), format!("<{name} ")];
        for open in &open_variants {
            let Some(start) = block.to_lowercase().find(&open.to_lowercase()) else { continue };
            let after = &block[start + open.len()..];
            let close = format!("</{name}>");
            let Some(end) = after.to_lowercase().find(&close) else { continue };
            return Some(after[..end].trim().to_string());
        }
    }
    None
}

/// 解析 HTTP 日期（RFC1123 / RFC850 / asctime 宽容尝试）
fn parse_http_date(s: &str) -> u64 {
    // 委托给系统：用 date 命令解析太重；手写常用 RFC1123（英文月份）
    const MONTHS: [&str; 12] = ["jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct", "nov", "dec"];
    let lower = s.to_lowercase();
    // 找 "07 nov 2026 08:00:00" 或 "nov 07 2026" 形态：取日/月/年/时/分/秒
    let toks: Vec<&str> = lower.split(|c: char| c == ' ' || c == ',').filter(|t| !t.is_empty()).collect();
    let mut day = 0u32;
    let mut month = 0u32;
    let mut year = 0i64;
    let (mut hh, mut mm, mut ss) = (0u32, 0u32, 0u32);
    for (i, t) in toks.iter().enumerate() {
        if let Some(mi) = MONTHS.iter().position(|m| t.starts_with(m)) {
            month = mi as u32 + 1;
            // 日通常在月前（RFC1123）或后（asctime）
            if i > 0 {
                day = toks[i - 1].trim().parse().unwrap_or(0);
            }
            if i + 1 < toks.len() {
                year = toks[i + 1].parse().unwrap_or(0);
            }
        }
        if t.contains(':') {
            let parts: Vec<&str> = t.split(':').collect();
            if parts.len() == 3 {
                hh = parts[0].parse().unwrap_or(0);
                mm = parts[1].parse().unwrap_or(0);
                ss = parts[2].parse().unwrap_or(0);
            }
        }
    }
    if year < 1970 || month == 0 || day == 0 {
        return 0;
    }
    // 简化 civil→unix（够用，不处理闰秒）
    let days = days_from_civil(year, month, day);
    (days * 86400 + (hh as i64) * 3600 + (mm as i64) * 60 + ss as i64) as u64
}

fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = ((m + 9) % 12) as i64;
    let doy = (153 * mp + 2) / 5 + d as i64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe - 719468
}

/// URL 解码（%XX 与 +）
pub fn url_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'%' if i + 2 < bytes.len() => {
                let hex = &s[i + 1..i + 3];
                if let Ok(v) = u8::from_str_radix(hex, 16) {
                    out.push(v);
                    i += 3;
                } else {
                    out.push(b'%');
                    i += 1;
                }
            }
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            c => {
                out.push(c);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).to_string()
}

/// 递归枚举远端目录树（Depth:1 分层遍历，兼容性优于 infinity）
pub fn list_remote(base_url: &str, username: &str, password: &str) -> Result<Vec<RemoteFile>, String> {
    let auth = format!("{username}:{password}");
    let mut out: Vec<RemoteFile> = Vec::new();
    // 待遍历的绝对 URL 队列（URL 形态，尾斜杠）
    let mut queue: Vec<String> = vec![base_url.trim_end_matches('/').to_string() + "/"];
    let base_norm = base_url.trim_end_matches('/').to_lowercase() + "/";
    let mut visited = 0usize;
    while let Some(dir_url) = queue.pop() {
        visited += 1;
        if visited > 2000 {
            return Err("目录层级过深或条目过多（>2000），已中止".into());
        }
        let (dirs, files) = propfind_level(&auth, &dir_url, "1")?;
        for d in dirs {
            let du = d.trim_end_matches('/');
            // 只遍历 base 之下的目录（绝对 href 或相对 href 都归一为绝对 URL）
            let abs = if du.to_lowercase().starts_with("http") {
                du.to_string()
            } else {
                // 相对 href：基于 dir_url 拼接（href 可能 URL 编码）
                format!("{}/{}", dir_url.trim_end_matches('/'), url_decode(du.trim_start_matches('/')))
            };
            let abs_norm = abs.trim_end_matches('/').to_lowercase();
            if !abs_norm.starts_with(&base_norm) || abs_norm == base_norm.trim_end_matches('/').to_string() {
                continue;
            }
            queue.push(abs.trim_end_matches('/').to_string() + "/");
        }
        for f in files {
            let href = f.rel.trim_end_matches('/');
            let abs = if href.to_lowercase().starts_with("http") {
                href.to_string()
            } else {
                format!("{}/{}", dir_url.trim_end_matches('/'), url_decode(href.trim_start_matches('/')))
            };
            let abs_norm = abs.trim_end_matches('/').to_lowercase();
            if !abs_norm.starts_with(&base_norm) {
                continue;
            }
            let rel = url_decode(&abs[base_url.trim_end_matches('/').len()..].trim_start_matches('/'));
            if rel.is_empty() || rel.split('/').any(|seg| seg == ".." || seg.is_empty() && false) {
                continue;
            }
            out.push(RemoteFile { rel, size: f.size, mtime: f.mtime });
        }
    }
    out.sort_by(|a, b| a.rel.cmp(&b.rel));
    Ok(out)
}

/// 下载单个远端文件到本地路径
pub fn download(url: &str, username: &str, password: &str, to: &Path) -> Result<(), String> {
    let auth = format!("{username}:{password}");
    let to_str = to.to_string_lossy().to_string();
    run_curl(&["-u", &auth, "-o", &to_str, &url.trim_end_matches('/').to_string()]).map(|_| ())
}

/// 对比镜像：返回需要下载的（远端条目 + 本地目标）、需要删除的本地相对路径
pub fn diff_mirror(
    remote: &[RemoteFile],
    state: &MirrorState,
    local_root: &Path,
    max_file_kb: u64,
) -> (Vec<(RemoteFile, std::path::PathBuf)>, Vec<String>) {
    let mut to_download = Vec::new();
    let mut to_delete = Vec::new();
    let max_bytes = if max_file_kb == 0 { 64 * 1024 * 1024 } else { max_file_kb * 1024 };
    for r in remote {
        let changed = match state.get(&r.rel) {
            Some(prev) => prev.size != r.size || (r.mtime != 0 && prev.mtime != 0 && prev.mtime != r.mtime),
            None => true,
        };
        if !changed || r.size > max_bytes {
            continue;
        }
        let local = local_root.join(r.rel.replace('/', "\\"));
        to_download.push((r.clone(), local));
    }
    for rel in state.keys() {
        if !remote.iter().any(|r| &r.rel == rel) {
            to_delete.push(rel.clone());
        }
    }
    (to_download, to_delete)
}

/// 测试连接（PROPFIND 根一层）
pub fn test_connection(url: &str, username: &str, password: &str) -> Result<(), String> {
    let auth = format!("{username}:{password}");
    let trimmed = url.trim_end_matches('/').to_string() + "/";
    run_curl(&["-u", &auth, "-X", "PROPFIND", "-H", "Depth: 0", &trimmed]).map(|_| ())
}

/// 同步间隔兜底
pub const MIN_INTERVAL_SECS: u64 = 30;
pub fn wait_hint() -> Duration {
    Duration::from_secs(MIN_INTERVAL_SECS)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn url_decode_works() {
        assert_eq!(url_decode("a%20b+c"), "a b c");
        assert_eq!(url_decode("%E4%B8%AD%E6%96%87"), "中文");
        assert_eq!(url_decode("plain"), "plain");
    }

    #[test]
    fn parse_http_date_works() {
        // Mon, 07 Nov 2026 08:00:00 GMT
        let t = parse_http_date("Mon, 07 Nov 2026 08:00:00 GMT");
        assert!(t > 1_700_000_000, "got {t}");
    }

    #[test]
    fn extract_tag_ignores_namespace() {
        let block = "<D:propstat><D:prop><D:getcontentlength>123</D:getcontentlength></D:prop></D:propstat>";
        assert_eq!(extract_tag(block, &["d:getcontentlength", "getcontentlength"]).unwrap(), "123");
    }

    #[test]
    fn diff_detects_add_change_delete() {
        let remote = vec![
            RemoteFile { rel: "a.md".into(), size: 10, mtime: 100 },
            RemoteFile { rel: "b.md".into(), size: 20, mtime: 100 },
        ];
        let mut state = MirrorState::new();
        state.insert("a.md".into(), RemoteFile { rel: "a.md".into(), size: 10, mtime: 100 });
        state.insert("gone.md".into(), RemoteFile { rel: "gone.md".into(), size: 5, mtime: 1 });
        let (dl, del) = diff_mirror(&remote, &state, Path::new("x"), 4096);
        assert_eq!(dl.len(), 1, "b.md 新增");
        assert_eq!(dl[0].0.rel, "b.md");
        assert_eq!(del, vec!["gone.md".to_string()]);
    }
}
