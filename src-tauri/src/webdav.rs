//! WebDAV 远程目录源 —— 通过系统 curl CLI 完成枚举与下载（零新增 Rust 依赖）
//!
//! 流程：PROPFIND Depth:1 逐层递归收集远端 {rel, size, mtime}
//!       → 与本地镜像元数据对比 → 新增/变更 GET 下载、多余删除
//! 安全：凭据仅作为 curl -u 参数传给子进程；镜像路径由 sanitize 后的
//!       仓库名锚定，rel 中的 `..` 段与反斜杠直接拒绝（防穿越）。

use crate::safe_path::{safe_join_under, safe_relative_path};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
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
    // 只折叠 ASCII 字节，保持 UTF-8 字节长度不变；Unicode to_lowercase()
    // 可能扩展字符长度，拿变化后的偏移切原 XML 会 panic。
    let bytes = body.as_bytes();
    if find_ascii_case_insensitive(bytes, b"response").is_none() {
        return Err(format!("PROPFIND 响应异常（HTTP 状态/认证失败?）: {}", &body.chars().take(200).collect::<String>()));
    }
    let mut dirs = Vec::new();
    let mut files = Vec::new();
    // 逐个 <...:response> 块解析（保持原始大小写提取 href）
    let mut cursor = 0usize;
    while let Some(pos) = find_ascii_case_insensitive(&bytes[cursor..], b"<").map(|p| p + cursor) {
        let end = find_ascii_case_insensitive(&bytes[pos..], b">").map(|p| p + pos + 1).unwrap_or(bytes.len());
        let tag_start = &bytes[pos + 1..end.min(bytes.len())];
        let is_d_response = starts_with_ascii_case_insensitive(tag_start, b"d:response");
        let is_response = is_d_response || starts_with_ascii_case_insensitive(tag_start, b"response");
        if is_response && !starts_with_ascii_case_insensitive(tag_start, b"/") {
            // 找到 response 开块：截到对应结束标签
            let close_pat: &[u8] = if is_d_response { b"</d:response>" } else { b"</response>" };
            let close = find_ascii_case_insensitive(&bytes[pos..], close_pat).map(|p| pos + p);
            let Some(close_pos) = close else { break };
            let block = &body[pos..close_pos];

            let href = extract_tag(block, &["d:href", "href"]).unwrap_or_default();
            let is_dir = find_ascii_case_insensitive(block.as_bytes(), b"collection").is_some();
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

fn starts_with_ascii_case_insensitive(haystack: &[u8], needle: &[u8]) -> bool {
    haystack.len() >= needle.len()
        && haystack[..needle.len()]
            .iter()
            .zip(needle)
            .all(|(a, b)| a.eq_ignore_ascii_case(b))
}

fn find_ascii_case_insensitive(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() {
        return Some(0);
    }
    haystack
        .windows(needle.len())
        .position(|window| starts_with_ascii_case_insensitive(window, needle))
}

/// 宽松提取首个 <tag>value</tag>（忽略命名空间前缀与大小写）
fn extract_tag(block: &str, names: &[&str]) -> Option<String> {
    for name in names {
        let open_variants = [format!("<{name}>"), format!("<{name} ")];
        for open in &open_variants {
            let Some(start) = find_ascii_case_insensitive(block.as_bytes(), open.as_bytes()) else { continue };
            let after = &block[start + open.len()..];
            let close = format!("</{name}>");
            let Some(end) = find_ascii_case_insensitive(after.as_bytes(), close.as_bytes()) else { continue };
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

fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

/// URL 解码（%XX 与 +）
pub fn url_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'%' if i + 2 < bytes.len() => {
                if let (Some(hi), Some(lo)) = (hex_value(bytes[i + 1]), hex_value(bytes[i + 2])) {
                    out.push((hi << 4) | lo);
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

fn encode_url_path_segment(segment: &str) -> String {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    let mut encoded = String::with_capacity(segment.len());
    for byte in segment.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
            encoded.push(char::from(byte));
        } else {
            encoded.push('%');
            encoded.push(char::from(HEX[(byte >> 4) as usize]));
            encoded.push(char::from(HEX[(byte & 0x0f) as usize]));
        }
    }
    encoded
}

fn encode_relative_url_path(rel: &str) -> Result<String, String> {
    safe_relative_path(rel)?;
    Ok(rel
        .split('/')
        .map(encode_url_path_segment)
        .collect::<Vec<_>>()
        .join("/"))
}

/// 将已解码、已校验的远端相对路径重新编码为 GET URL，避免 `%`、`?`、`#`
/// 和 Unicode 被服务端二次解释；路径段分隔符 `/` 保持不变。
pub fn remote_file_url(base_url: &str, rel: &str) -> Result<String, String> {
    let base = base_url.split(['?', '#']).next().unwrap_or(base_url);
    Ok(format!(
        "{}/{}",
        base.trim_end_matches('/'),
        encode_relative_url_path(rel)?
    ))
}

/// 将绝对 WebDAV URL 收敛为集合内的安全相对路径。
/// 集合判断按完整路径段进行，`/dav-evil` 不会被误认为 `/dav` 的子路径。
fn remote_rel_within_base(abs: &str, origin: &str, base_path: &str) -> Result<String, String> {
    let without_query = abs.split(['?', '#']).next().unwrap_or(abs);
    if !without_query
        .get(..origin.len())
        .map(|prefix| prefix.eq_ignore_ascii_case(origin))
        .unwrap_or(false)
    {
        return Err(format!("WebDAV 条目不在预期 origin 内: {abs}"));
    }

    let raw_path = without_query
        .get(origin.len()..)
        .ok_or_else(|| format!("WebDAV 条目不在预期 origin 内: {abs}"))?;
    // 原始 origin 后必须紧跟路径边界。若先解码，%2F 可把 host 的一部分
    // 变成 `/`，让字符串前缀检查通过但实际 URL 主机已经改变。
    if !raw_path.is_empty() && !raw_path.starts_with('/') {
        return Err(format!("WebDAV 条目 origin 后缺少路径边界: {abs}"));
    }
    let decoded_path = url_decode(raw_path);
    let decoded_base = url_decode(base_path).trim_end_matches('/').to_string();
    // 根集合 https://host/ 的绝对自引用在 normalize 后可能没有 path。
    if decoded_path.is_empty() && decoded_base.is_empty() {
        return Ok(String::new());
    }
    if !decoded_path.starts_with('/') {
        return Err(format!("WebDAV 条目不是绝对路径: {abs}"));
    }
    // origin 主机名不区分大小写，但 URL path 必须按服务器语义区分大小写。
    if !decoded_base.is_empty() && !decoded_path.starts_with(&decoded_base) {
        return Err(format!("WebDAV 条目不在预期集合内: {abs}"));
    }
    let rest = if decoded_base.is_empty() {
        decoded_path.as_str()
    } else {
        let rest = decoded_path
            .get(decoded_base.len()..)
            .ok_or_else(|| format!("WebDAV 集合路径边界异常: {abs}"))?;
        if !rest.is_empty() && !rest.starts_with('/') {
            return Err(format!("WebDAV 集合前缀缺少路径段边界: {abs}"));
        }
        rest
    };

    // 只移除集合边界的一个 `/`；`//server/share` 必须保留前导斜杠，
    // 交由 safe_relative_path 作为绝对路径/UNC 拒绝，不能静默归一化。
    let rel = rest.strip_prefix('/').unwrap_or(rest).to_string();
    if rel.is_empty() {
        return Ok(String::new());
    }
    safe_relative_path(&rel)?;
    Ok(rel)
}

fn directory_visit_key(url: &str) -> String {
    let trimmed = url.trim_end_matches('/');
    if let Some(scheme_end) = trimmed.find("://") {
        let host_start = scheme_end + 3;
        if let Some(path_offset) = trimmed[host_start..].find('/') {
            let origin_end = host_start + path_offset;
            // scheme/host 不区分大小写；URL path 按 WebDAV 语义保持大小写。
            return format!("{}{}", trimmed[..origin_end].to_ascii_lowercase(), &trimmed[origin_end..]);
        }
    }
    trimmed.to_ascii_lowercase()
}

/// 递归枚举远端目录树（Depth:1 分层遍历，兼容性优于 infinity）
pub fn list_remote(base_url: &str, username: &str, password: &str) -> Result<Vec<RemoteFile>, String> {
    let auth = format!("{username}:{password}");
    // origin = scheme://host:port；base_path = 集合路径（如 /dav）
    let trimmed = base_url.trim_end_matches('/');
    let origin_end = trimmed.find("://").map(|p| trimmed[p + 3..].find('/').map(|q| p + 3 + q)).flatten().unwrap_or(trimmed.len());
    let origin = trimmed[..origin_end].to_string();
    let base_path = trimmed[origin_end..].to_string();

    let mut out: Vec<RemoteFile> = Vec::new();
    let mut queue: Vec<String> = vec![trimmed.to_string() + "/"];
    // 防环：服务器可能把当前集合自身也列进 Depth:1 响应（RFC 允许）
    let mut visited: std::collections::HashSet<String> = std::collections::HashSet::new();
    while let Some(dir_url) = queue.pop() {
        let key = directory_visit_key(&dir_url);
        if !visited.insert(key) {
            continue;
        }
        if visited.len() > 2000 {
            return Err("目录层级过深或条目过多（>2000），已中止".into());
        }
        let (dirs, files) = propfind_level(&auth, &dir_url, "1")?;
        // href 归一为绝对 URL：WebDAV 规范返回以 / 开头的绝对路径（已含集合前缀）
        let normalize = |href: &str| -> Result<String, String> {
            let h = href.trim_end_matches('/');
            if h.get(..4).map(|prefix| prefix.eq_ignore_ascii_case("http")).unwrap_or(false) {
                return Ok(h.to_string());
            }
            // 保留原始编码供 remote_rel_within_base 校验；实际入队 URL 再由
            // remote_file_url 从解码后的安全相对路径统一重新编码。
            if h.starts_with('/') {
                Ok(origin.clone() + h)
            } else {
                Ok(format!("{}/{}", dir_url.trim_end_matches('/'), h))
            }
        };
        for d in &dirs {
            let abs = normalize(d)?;
            let rel = remote_rel_within_base(&abs, &origin, &base_path)?;
            if !rel.is_empty() {
                queue.push(remote_file_url(base_url, &rel)? + "/");
            }
        }
        for f in &files {
            let abs = normalize(&f.rel)?;
            let rel = remote_rel_within_base(&abs, &origin, &base_path)?;
            if !rel.is_empty() {
                out.push(RemoteFile { rel, size: f.size, mtime: f.mtime });
            }
        }
    }
    out.sort_by(|a, b| a.rel.cmp(&b.rel));
    // Windows 镜像目录大小写不敏感；同一远端集合中的冲突路径必须显式失败，
    // 不能下载两次覆盖同一文件或在删除阶段产生不确定行为。
    crate::safe_path::validate_relative_paths_unique(
        out.iter().map(|remote| remote.rel.as_str()),
    )?;
    Ok(out)
}

/// 下载单个远端文件到本地路径
pub fn download(url: &str, username: &str, password: &str, to: &Path) -> Result<(), String> {
    let auth = format!("{username}:{password}");
    let to_str = to.to_string_lossy().to_string();
    run_curl(&["-u", &auth, "-o", &to_str, &url.trim_end_matches('/').to_string()]).map(|_| ())
}

/// 将远端相对路径收敛到本地镜像根内；下载与删除必须共用此入口。
pub fn local_path(root: &Path, rel: &str) -> Result<PathBuf, String> {
    safe_join_under(root, rel)
}

/// 对比镜像：返回需要下载的（远端条目 + 本地目标）、需要删除的本地相对路径。
/// 非法远端路径使本次同步失败；非法旧元数据只告警并跳过删除。
pub fn diff_mirror(
    remote: &[RemoteFile],
    state: &MirrorState,
    local_root: &Path,
    max_file_kb: u64,
) -> Result<(Vec<(RemoteFile, PathBuf)>, Vec<String>), String> {
    let mut to_download = Vec::new();
    let mut to_delete = Vec::new();
    // diff 也必须独立于 list_remote 拒绝同一 Windows 文件上的大小写别名。
    crate::safe_path::validate_relative_paths_unique(
        remote.iter().map(|remote| remote.rel.as_str()),
    )?;
    let max_bytes = if max_file_kb == 0 { 64 * 1024 * 1024 } else { max_file_kb * 1024 };
    for r in remote {
        // 即使条目未变化也先校验，避免恶意元数据长期留在镜像状态中。
        safe_relative_path(&r.rel)?;
        // Windows 上 A.md/a.md 是同一文件：旧元数据仅大小写不同也视为同一项，
        // 避免“先下载新名、再删除旧名”把刚下载的文件删掉。
        let previous = state.get(&r.rel).or_else(|| {
            state
                .iter()
                .find(|(rel, _)| crate::safe_path::paths_case_collide(rel, &r.rel))
                .map(|(_, remote)| remote)
        });
        let changed = match previous {
            Some(prev) => prev.size != r.size || (r.mtime != 0 && prev.mtime != 0 && prev.mtime != r.mtime),
            None => true,
        };
        if !changed || r.size > max_bytes {
            continue;
        }
        let local = local_path(local_root, &r.rel)?;
        to_download.push((r.clone(), local));
    }
    for rel in state.keys() {
        // 同路径或大小写别名都表示远端仍存在，不得作为删除候选。
        if remote
            .iter()
            .any(|r| crate::safe_path::paths_case_collide(&r.rel, rel))
        {
            continue;
        }
        match local_path(local_root, rel) {
            Ok(_) => to_delete.push(rel.clone()),
            Err(e) => crate::kdlog!(warn, "webdav", "跳过非法旧镜像路径 {rel:?}: {e}"),
        }
    }
    Ok((to_download, to_delete))
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

// ---------------- DPAPI 凭据加密 ----------------

/// 加密标记前缀（config 中的 password 形态）
pub const DPAPI_PREFIX: &str = "dpapi:";

fn run_powershell(script: &str) -> Result<String, String> {
    let out = std::process::Command::new("powershell")
        .args(["-NoProfile", "-NonInteractive", "-Command", script])
        .output()
        .map_err(|e| format!("无法执行 powershell: {e}"))?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
    } else {
        Err(format!(
            "powershell 失败: {}",
            String::from_utf8_lossy(&out.stderr).trim().chars().take(150).collect::<String>()
        ))
    }
}

/// DPAPI（CurrentUser）加密：返回 "dpapi:<base64>"；明文经 stdin 传入避免转义问题
pub fn dpapi_encrypt(plain: &str) -> Result<String, String> {
    use std::io::Write;
    let mut child = std::process::Command::new("powershell")
        .args(["-NoProfile", "-NonInteractive", "-Command",
            "Add-Type -AssemblyName System.Security;\
             $s=[Console]::In.ReadToEnd();\
             $b=[Text.Encoding]::UTF8.GetBytes($s);\
             $p=[Security.Cryptography.ProtectedData]::Protect($b,$null,[Security.Cryptography.DataProtectionScope]::CurrentUser);\
             [Convert]::ToBase64String($p)"])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| format!("无法执行 powershell: {e}"))?;
    child
        .stdin
        .as_mut()
        .unwrap()
        .write_all(plain.as_bytes())
        .map_err(|e| format!("写入 DPAPI 输入失败: {e}"))?;
    child
        .stdin
        .as_mut()
        .unwrap()
        .flush()
        .map_err(|e| format!("刷新 DPAPI 输入失败: {e}"))?;
    drop(child.stdin.take());
    let out = child.wait_with_output().map_err(|e| e.to_string())?;
    if !out.status.success() {
        return Err(format!(
            "DPAPI 加密失败: {}",
            String::from_utf8_lossy(&out.stderr).trim().chars().take(150).collect::<String>()
        ));
    }
    let cipher = String::from_utf8_lossy(&out.stdout).trim().to_string();
    Ok(format!("{DPAPI_PREFIX}{cipher}"))
}

/// DPAPI 解密："dpapi:<base64>" → 明文；无前缀 = 旧版明文直接返回
pub fn dpapi_decrypt(stored: &str) -> Result<String, String> {
    let Some(cipher) = stored.strip_prefix(DPAPI_PREFIX) else {
        return Ok(stored.to_string());
    };
    let script = format!(
        "Add-Type -AssemblyName System.Security;\
         $b=[Convert]::FromBase64String('{cipher}');\
         $p=[Security.Cryptography.ProtectedData]::Unprotect($b,$null,[Security.Cryptography.DataProtectionScope]::CurrentUser);\
         [Text.Encoding]::UTF8.GetString($p)"
    );
    run_powershell(&script)
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
    fn path_security_url_decode_handles_unicode_without_slicing_panic() {
        assert_eq!(url_decode("%aé"), "%aé");
        assert_eq!(url_decode("%E4%B8%AD%E6%96%87"), "中文");
    }

    #[test]
    fn path_security_remote_file_url_reencodes_decoded_path() {
        assert_eq!(
            remote_file_url("https://dav.example/dav", "docs/a b%#中文.md").unwrap(),
            "https://dav.example/dav/docs/a%20b%25%23%E4%B8%AD%E6%96%87.md"
        );
        assert_eq!(
            remote_file_url("https://dav.example/dav", "%2e%2e%2fsecret").unwrap(),
            "https://dav.example/dav/%252e%252e%252fsecret"
        );
        assert!(remote_file_url("https://dav.example/dav", "../secret").is_err());
    }

    #[test]
    fn path_security_directory_visit_key_preserves_path_case() {
        assert_eq!(
            directory_visit_key("HTTPS://Example.COM:443/dav/A/"),
            directory_visit_key("https://example.com:443/dav/A")
        );
        assert_ne!(
            directory_visit_key("https://example.com/dav/A/"),
            directory_visit_key("https://example.com/dav/a/")
        );
    }

    #[test]
    fn path_security_xml_extract_preserves_utf8_offsets() {
        let block = "<!-- İ --><D:prop><D:href>中文/a.md</D:href><D:getcontentlength>7</D:getcontentlength></D:prop>";
        assert_eq!(extract_tag(block, &["d:href", "href"]).unwrap(), "中文/a.md");
        assert_eq!(extract_tag(block, &["d:getcontentlength", "getcontentlength"]).unwrap(), "7");
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
        let (dl, del) = diff_mirror(&remote, &state, Path::new("x"), 4096).unwrap();
        assert_eq!(dl.len(), 1, "b.md 新增");
        assert_eq!(dl[0].0.rel, "b.md");
        assert_eq!(del, vec!["gone.md".to_string()]);
    }

    #[test]
    fn path_security_remote_collection_boundary() {
        let origin = "https://dav.example.com";
        assert_eq!(
            remote_rel_within_base(
                "https://dav.example.com/dav/docs/a%20b.md",
                origin,
                "/dav"
            )
            .unwrap(),
            "docs/a b.md"
        );
        assert!(remote_rel_within_base(
            "https://dav.example.com/dav-evil/a.md",
            origin,
            "/dav"
        )
        .is_err());
        assert!(remote_rel_within_base(
            "https://dav.example.com/dav/..%5Cescape.md",
            origin,
            "/dav"
        )
        .is_err());
        assert!(remote_rel_within_base(
            "https://evil.example.com/dav/a.md",
            origin,
            "/dav"
        )
        .is_err());
        assert!(remote_rel_within_base(
            "https://dav.example.com%2F@evil.example/a.md",
            origin,
            ""
        )
        .is_err());
        assert!(remote_rel_within_base(
            "https://dav.example.com/DAV/secret.md",
            origin,
            "/dav"
        )
        .is_err());
        assert!(remote_rel_within_base(
            "https://dav.example.com/dav/C:%5Cescape.md",
            origin,
            "/dav"
        )
        .is_err());
        assert!(remote_rel_within_base(
            "https://dav.example.com/dav/%2F%2Fserver/share/a.md",
            origin,
            "/dav"
        )
        .is_err());
        assert!(remote_rel_within_base(
            "https://dav.example.com/dav//a.md",
            origin,
            "/dav"
        )
        .is_err());
    }

    #[test]
    fn path_security_remote_root_collection_self_reference() {
        let origin = "https://dav.example.com";
        assert_eq!(remote_rel_within_base(origin, origin, "").unwrap(), "");
        assert_eq!(remote_rel_within_base("https://dav.example.com/", origin, "").unwrap(), "");
        assert_eq!(
            remote_rel_within_base("https://dav.example.com/docs/a.md", origin, "").unwrap(),
            "docs/a.md"
        );
    }

    #[test]
    fn path_security_diff_treats_case_aliases_as_same_windows_file() {
        let mut state = MirrorState::new();
        state.insert(
            "docs/A.md".into(),
            RemoteFile {
                rel: "docs/A.md".into(),
                size: 7,
                mtime: 10,
            },
        );
        let remote = vec![RemoteFile {
            rel: "docs/a.md".into(),
            size: 8,
            mtime: 11,
        }];
        let (downloads, deletes) =
            diff_mirror(&remote, &state, Path::new("mirror"), 4096).unwrap();
        assert_eq!(downloads.len(), 1);
        assert_eq!(downloads[0].0.rel, "docs/a.md");
        assert!(deletes.is_empty(), "旧大小写别名不能作为删除项");

        let duplicate = vec![remote[0].clone(), RemoteFile {
            rel: "docs/A.md".into(),
            size: 8,
            mtime: 11,
        }];
        assert!(diff_mirror(&duplicate, &state, Path::new("mirror"), 4096).is_err());
    }

    #[test]
    fn path_security_diff_rejects_remote_escape_and_skips_bad_state() {
        let remote = vec![RemoteFile {
            rel: "..\\escape.md".into(),
            size: 1,
            mtime: 1,
        }];
        assert!(diff_mirror(&remote, &MirrorState::new(), Path::new("x"), 4096).is_err());

        let mut state = MirrorState::new();
        state.insert(
            "..\\escape.md".into(),
            RemoteFile {
                rel: "..\\escape.md".into(),
                size: 1,
                mtime: 1,
            },
        );
        let (downloads, deletes) =
            diff_mirror(&[], &state, Path::new("x"), 4096).unwrap();
        assert!(downloads.is_empty());
        assert!(deletes.is_empty());
    }
}
