//! AI 引擎（Phase 1：结果筛选/重排）——OpenAI 兼容 /chat/completions 的 curl CLI 客户端。
//! 沿用 webdav.rs 的 curl 零 Rust 依赖模式；payload 经 stdin 传递避免命令行长度/转义问题。

use serde_json::{json, Value};
use std::io::Write;
use std::process::{Command, Stdio};

/// 单次 chat/completions 调用（非流式），返回 content 文本
pub fn chat(
    base_url: &str,
    api_key: &str,
    model: &str,
    timeout_ms: u64,
    system: &str,
    user: &str,
) -> Result<String, String> {
    let base = base_url.trim().trim_end_matches('/').to_string();
    if base.is_empty() {
        return Err("AI base_url 未配置".into());
    }
    if model.trim().is_empty() {
        return Err("AI model 未配置".into());
    }
    let payload = json!({
        "model": model.trim(),
        "messages": [
            { "role": "system", "content": system },
            { "role": "user", "content": user }
        ],
        "temperature": 0.0,
        "stream": false
    })
    .to_string();

    let url = format!("{base}/chat/completions");
    let mut args: Vec<String> = vec![
        "-sS".into(),
        "--max-time".into(),
        (timeout_ms.max(1000) / 1000).to_string(),
        url,
        "-H".into(),
        "Content-Type: application/json".into(),
        "--data-binary".into(),
        "@-".into(),
    ];
    let key = api_key.trim();
    if !key.is_empty() {
        args.push("-H".into());
        args.push(format!("Authorization: Bearer {key}"));
    }

    let mut child = Command::new("curl")
        .args(&args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("启动 curl 失败: {e}"))?;
    // stdin 写入失败不致命（curl 可能已退出）
    let _ = child.stdin.take().unwrap().write_all(payload.as_bytes());
    let out = child
        .wait_with_output()
        .map_err(|e| format!("curl 执行失败: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "curl 退出 {}: {}",
            out.status,
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    let body = String::from_utf8_lossy(&out.stdout);
    let v: Value =
        serde_json::from_str(&body).map_err(|e| format!("响应非 JSON: {e}; 前200字: {}", &body[..body.len().min(200)]))?;
    let content = v
        .pointer("/choices/0/message/content")
        .and_then(|c| c.as_str())
        .ok_or_else(|| format!("响应缺少 choices[0].message.content: {}", &body[..body.len().min(200)]))?
        .to_string();
    Ok(content)
}

/// 结果筛选：把 (编号, 文本) 候选交给模型，返回"相关"编号子集（保持传入顺序）
pub fn rerank(
    base_url: &str,
    api_key: &str,
    model: &str,
    timeout_ms: u64,
    query: &str,
    items: &[(usize, String)],
) -> Result<Vec<usize>, String> {
    if items.is_empty() {
        return Ok(Vec::new());
    }
    let mut listing = String::new();
    for (idx, text) in items {
        let t = text.trim().chars().take(300).collect::<String>();
        listing.push_str(&format!("[{idx}] {t}\n"));
    }
    let system = "你是本地知识库搜索的相关性过滤器。给定用户查询和编号候选段落，只输出与查询真正相关的候选编号。\
只输出一个 JSON 对象，格式：{\"keep\":[编号,...]}，不要输出任何解释文字。若全部不相关输出 {\"keep\":[]}。";
    let user = format!("查询：{query}\n\n候选：\n{listing}");
    let content = chat(base_url, api_key, model, timeout_ms, system, &user)?;
    // 宽松提取第一个 { ... } JSON（模型可能带 ```json 围栏）
    let s = content.trim();
    let json_str = match (s.find('{'), s.rfind('}')) {
        (Some(a), Some(b)) if b > a => &s[a..=b],
        _ => return Err(format!("AI 未返回 JSON: {}", &s[..s.len().min(200)])),
    };
    let v: Value =
        serde_json::from_str(json_str).map_err(|e| format!("AI JSON 解析失败: {e}; 原文: {json_str}"))?;
    let keep = v
        .get("keep")
        .and_then(|k| k.as_array())
        .ok_or("AI JSON 缺少 keep 数组")?;
    let valid: std::collections::HashSet<usize> = items.iter().map(|(i, _)| *i).collect();
    let mut out = Vec::new();
    for x in keep {
        if let Some(n) = x.as_u64() {
            let n = n as usize;
            if valid.contains(&n) && !out.contains(&n) {
                out.push(n);
            }
        }
    }
    Ok(out)
}
