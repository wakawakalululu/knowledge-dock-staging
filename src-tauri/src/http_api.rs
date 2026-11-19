//! 本地 HTTP 检索接口：供其它程序（如 CTF 答题脚本）直接调用搜索引擎。
//! 仅绑定 127.0.0.1；std::net 零依赖实现，路由：
//!   GET /api/search?q=...&limit=&type=&root=   → {"ok":true,"total":N,"hits":[...]}
//!   GET /api/stats                              → 索引统计
//!   GET /api/health                             → {"ok":true}
//! token 非空时要求 ?token= 或 Authorization: Bearer 匹配。

use crate::ai;
use crate::engine::Engine;
use tauri::Manager;
use serde_json::json;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::Arc;

/// 启动 API 服务线程（端口绑定失败仅打日志，不影响主应用）
pub fn spawn(app: tauri::AppHandle, engine: Arc<Engine>, port: u16, token: String) {
    std::thread::spawn(move || {
        let listener = match TcpListener::bind(("127.0.0.1", port)) {
            Ok(l) => l,
            Err(e) => {
                crate::kdlog!(error, "api", "127.0.0.1:{port} 绑定失败（端口被占用或被禁用）: {e}");
                return;
            }
        };
        crate::kdlog!(info, "api", "HTTP 检索接口已启动 http://127.0.0.1:{port}/api/search");
        for stream in listener.incoming() {
            match stream {
                Ok(s) => {
                    let app = app.clone();
                    let engine = engine.clone();
                    let token = token.clone();
                    std::thread::spawn(move || {
                        // handler panic 不拖垮进程（连接断开，服务继续）
                        let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                            let _ = handle(s, app, engine, token);
                        }));
                        if r.is_err() {
                            crate::kdlog!(error, "api", "请求处理线程 panic，连接已断开");
                        }
                    });
                }
                Err(_) => continue,
            }
        }
    });
}

fn handle(mut stream: TcpStream, app: tauri::AppHandle, engine: Arc<Engine>, token: String) -> std::io::Result<()> {
    // 读请求头（读到空行即止；忽略 body）
    let mut buf = Vec::new();
    let mut byte = [0u8; 1];
    loop {
        match stream.read(&mut byte) {
            Ok(0) => break,
            Ok(_) => {
                buf.push(byte[0]);
                if buf.ends_with(b"\r\n\r\n") || buf.len() > 16 * 1024 {
                    break;
                }
            }
            Err(_) => return Ok(()),
        }
    }
    let head = String::from_utf8_lossy(&buf);
    let request_line = head.lines().next().unwrap_or("").to_string();
    // "GET /api/search?q=.. HTTP/1.1"
    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or("");
    let target = parts.next().unwrap_or("");
    if method != "GET" {
        return respond(&mut stream, 405, &json!({"ok": false, "error": "method not allowed"}));
    }
    let (path, query) = match target.split_once('?') {
        Some((p, q)) => (p.to_string(), q.to_string()),
        None => (target.to_string(), String::new()),
    };
    let params = parse_params(&query);

    // 鉴权：token 非空时校验 ?token= 或 Authorization 头
    if !token.is_empty() {
        let header_token = head
            .lines()
            .find(|l| l.to_lowercase().starts_with("authorization:"))
            .and_then(|l| l.split_once(':'))
            .map(|(_, v)| v.trim().trim_start_matches("Bearer ").to_string());
        let given = params.get("token").cloned().or(header_token).unwrap_or_default();
        if given != token {
            return respond(&mut stream, 401, &json!({"ok": false, "error": "invalid token"}));
        }
    }

    match path.as_str() {
        "/api/health" => respond(
            &mut stream,
            200,
            &json!({"ok": true, "service": "knowledge-dock-api"}),
        ),
        "/api/search" => {
            let q = params.get("q").cloned().unwrap_or_default();
            if q.trim().is_empty() {
                return respond(&mut stream, 400, &json!({"ok": false, "error": "q is required"}));
            }
            let limit = params
                .get("limit")
                .and_then(|v| v.parse::<usize>().ok())
                .map(|v| v.clamp(1, 100))
                .unwrap_or(20);
            let type_filter = params.get("type").filter(|s| !s.is_empty()).map(|s| s.as_str());
            let root_filter = params.get("root").filter(|s| !s.is_empty()).map(|s| s.as_str());
            let start = std::time::Instant::now();
            let mut resp =
                engine.search(&q, limit, type_filter, root_filter);
            resp.duration_ms = start.elapsed().as_millis() as u64;
            // ai=1 → 用配置的模型筛选（未启用/失败时静默返回全量，ai_used=false）
            let mut ai_used = false;
            let ai_on = params
                .get("ai")
                .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
                .unwrap_or(false);
            if ai_on && !resp.hits.is_empty() {
                let state = app.state::<crate::AppState>();
                let cfg_ai = state.cfg.lock().expect("cfg lock").ai.clone();
                if cfg_ai.enabled && !cfg_ai.base_url.trim().is_empty() {
                    let key = if cfg_ai.api_key.starts_with("dpapi:") {
                        match crate::webdav::dpapi_decrypt(&cfg_ai.api_key) {
                            Ok(k) => k,
                            Err(e) => {
                                crate::kdlog!(warn, "api", "AI key 解密失败: {e}");
                                String::new()
                            }
                        }
                    } else {
                        cfg_ai.api_key.clone()
                    };
                    if !key.starts_with("dpapi:") {
                        let take = cfg_ai.top_n.min(resp.hits.len());
                        let items: Vec<(usize, String)> = resp.hits.iter().take(take)
                            .enumerate()
                            .map(|(i, h)| (i, format!("{} {}", h.title, h.snippet)))
                            .collect();
                        let q2 = q.clone();
                        let b2 = cfg_ai.base_url.clone();
                        let m2 = cfg_ai.model.clone();
                        let t2 = cfg_ai.timeout_ms;
                        match ai::rerank(&b2, &key, &m2, t2, &q2, &items) {
                            Ok(keep) if !keep.is_empty() => {
                                let keep_set: std::collections::HashSet<usize> = keep.into_iter().collect();
                                resp.hits = resp.hits.into_iter().enumerate()
                                    .filter(|(i, _)| keep_set.contains(i))
                                    .map(|(_, h)| h)
                                    .collect();
                                ai_used = true;
                            }
                            Ok(_) => { crate::kdlog!(warn, "api", "AI 筛选返回空 keep，保留全量"); }
                            Err(e) => { crate::kdlog!(warn, "api", "AI 筛选失败: {e}"); }
                        }
                    }
                }
            }
            respond(
                &mut stream,
                200,
                &json!({
                    "ok": true,
                    "total": resp.hits.len(),
                    "query": q,
                    "terms": resp.terms,
                    "duration_ms": resp.duration_ms,
                    "ai_used": ai_used,
                    "hits": resp.hits,
                }),
            )
        }
        "/api/logs" => {
            let n = params.get("lines").and_then(|v| v.parse::<usize>().ok()).map(|v| v.clamp(1, 2000)).unwrap_or(200);
            let text = crate::logging::read_tail(n);
            respond(&mut stream, 200, &json!({"ok": true, "lines": text.lines().count(), "tail": text}))
        }
        "/api/stats" => {
            let d = engine.stats_detail();
            respond(&mut stream, 200, &json!({"ok": true, "stats": d}))
        }
        _ => respond(&mut stream, 404, &json!({"ok": false, "error": "not found"})),
    }
}

fn respond(stream: &mut TcpStream, code: u16, body: &serde_json::Value) -> std::io::Result<()> {
    let payload = serde_json::to_string(body).unwrap_or_else(|_| "{}".into());
    let reason = match code {
        200 => "OK",
        400 => "Bad Request",
        401 => "Unauthorized",
        404 => "Not Found",
        405 => "Method Not Allowed",
        _ => "Error",
    };
    let resp = format!(
        "HTTP/1.1 {code} {reason}\r\nContent-Type: application/json; charset=utf-8\r\nContent-Length: {}\r\nAccess-Control-Allow-Origin: *\r\nConnection: close\r\n\r\n{payload}",
        payload.len()
    );
    stream.write_all(resp.as_bytes())?;
    stream.flush()
}

/// 查询串解析（支持 %XX 与 + 空格；UTF-8 安全）
fn parse_params(query: &str) -> std::collections::HashMap<String, String> {
    let mut out = std::collections::HashMap::new();
    for pair in query.split('&') {
        if pair.is_empty() {
            continue;
        }
        let (k, v) = match pair.split_once('=') {
            Some((k, v)) => (k, v),
            None => (pair, ""),
        };
        out.insert(url_decode(k), url_decode(v));
    }
    out
}

fn url_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            b'%' => {
                // 需要后两位十六进制
                if i + 2 < bytes.len() {
                    if let (Some(h), Some(l)) = (hex(bytes[i + 1]), hex(bytes[i + 2])) {
                        out.push(h * 16 + l);
                        i += 3;
                        continue;
                    }
                }
                out.push(b'%');
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

fn hex(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}
