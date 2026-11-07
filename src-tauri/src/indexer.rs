//! 知识库索引与检索：扫描目录 → 按空行分块 → 内存全文打分检索
//!
//! 中文无需分词：子串匹配 + 空格多词全含计分，个人知识库量级下
//! 每次按键的全量内存扫描在 10ms 以内。

use serde::Serialize;
use std::collections::HashMap;
use std::path::Path;
use walkdir::WalkDir;

const EXTS: [&str; 3] = ["md", "markdown", "txt"];
/// 跳过的目录名（隐藏目录一律跳过）
const IGNORE_DIRS: [&str; 6] = [
    "node_modules", "target", "dist", "__pycache__", ".git", ".obsidian",
];
/// 单文件上限，超过则截断（防止超大日志文件拖慢索引）
const MAX_FILE_BYTES: u64 = 2 * 1024 * 1024;

#[derive(Debug, Clone, Serialize)]
pub struct Hit {
    /// 展示用相对路径
    pub path: String,
    /// 文档标题（首个 `# ` 标题，否则文件名）
    pub title: String,
    /// 命中片段（由前端做高亮）
    pub snippet: String,
    /// 片段起始行号（1-based）
    pub line: usize,
    #[serde(skip_serializing)]
    pub score: i32,
    /// 打开文件用的绝对路径
    pub abs: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct Stats {
    pub docs: usize,
    pub chunks: usize,
}

struct Doc {
    path: String,
    title: String,
    abs: String,
}

struct Chunk {
    doc: usize,
    line: usize,
    lower: String,
    /// 命中片段的字符偏移（lower 中的字节偏移，另存文本原文做片段裁剪）
    text: String,
}

pub struct Index {
    docs: Vec<Doc>,
    chunks: Vec<Chunk>,
}

fn is_ignored_dir(name: &str) -> bool {
    name.starts_with('.') || IGNORE_DIRS.contains(&name)
}

/// 取首个 `# ` 标题作为文档标题
fn extract_title(content: &str, fallback: &str) -> String {
    for line in content.lines().take(30) {
        let t = line.trim();
        if let Some(rest) = t.strip_prefix("# ") {
            let rest = rest.trim();
            if !rest.is_empty() {
                return rest.to_string();
            }
        }
    }
    fallback.to_string()
}

fn push_chunk(chunks: &mut Vec<Chunk>, doc: usize, line: usize, buf: &mut String) {
    if buf.trim().is_empty() {
        buf.clear();
        return;
    }
    let text = std::mem::take(buf);
    let lower = text.to_lowercase();
    chunks.push(Chunk { doc, line, text, lower });
}

impl Index {
    pub fn empty() -> Self {
        Index { docs: Vec::new(), chunks: Vec::new() }
    }

    pub fn build(root: &Path) -> Result<Index, String> {
        if !root.is_dir() {
            return Err(format!("目录不存在: {}", root.display()));
        }
        let mut docs = Vec::new();
        let mut chunks: Vec<Chunk> = Vec::new();

        for entry in WalkDir::new(root)
            .into_iter()
            .filter_entry(|e| e.depth() == 0 || !is_ignored_dir(&e.file_name().to_string_lossy()))
            .filter_map(|e| e.ok())
        {
            if !entry.file_type().is_file() {
                continue;
            }
            let ext = entry
                .path()
                .extension()
                .map(|e| e.to_string_lossy().to_lowercase());
            if !matches!(ext.as_deref(), Some(x) if EXTS.contains(&x)) {
                continue;
            }
            if entry.metadata().map(|m| m.len() > MAX_FILE_BYTES).unwrap_or(true) {
                continue;
            }
            // 以 UTF-8 容错读取（GBK 老文档可能乱码，按损失读取不中断索引）
            let raw = match std::fs::read(entry.path()) {
                Ok(b) => b,
                Err(_) => continue,
            };
            let content = String::from_utf8_lossy(&raw);

            let stem = entry
                .path()
                .file_stem()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_default();
            let title = extract_title(&content, &stem);
            let rel = entry
                .path()
                .strip_prefix(root)
                .unwrap_or(entry.path())
                .to_string_lossy()
                .replace('\\', "/");
            let abs = entry.path().to_string_lossy().to_string();
            let doc = docs.len();
            docs.push(Doc { path: rel, title, abs });

            // 按空行分块并记录起始行号
            let mut buf = String::new();
            let mut start_line = 1usize;
            let mut ln = 1usize;
            for line in content.lines() {
                if line.trim().is_empty() {
                    push_chunk(&mut chunks, doc, start_line, &mut buf);
                } else {
                    if buf.is_empty() {
                        start_line = ln;
                    } else {
                        buf.push('\n');
                    }
                    buf.push_str(line);
                }
                ln += 1;
            }
            push_chunk(&mut chunks, doc, start_line, &mut buf);
        }

        Ok(Index { docs, chunks })
    }

    pub fn stats(&self) -> Stats {
        Stats { docs: self.docs.len(), chunks: self.chunks.len() }
    }

    /// 统计某词在文本中出现次数（非重叠），并返回首次位置；上限 cap 防御超长词
    fn count_at(hay: &str, needle: &str, cap: i32) -> Option<(i32, usize)> {
        if needle.is_empty() {
            return None;
        }
        let mut count = 0i32;
        let mut first: Option<usize> = None;
        let mut from = 0usize;
        while count < cap {
            match hay[from..].find(needle) {
                Some(p) => {
                    let abs = from + p;
                    if first.is_none() {
                        first = Some(abs);
                    }
                    count += 1;
                    from = abs + needle.len();
                }
                None => break,
            }
        }
        first.map(|p| (count, p))
    }

    pub fn search(&self, query: &str) -> Vec<Hit> {
        let q = query.trim();
        if q.is_empty() {
            return Vec::new();
        }
        let ql = q.to_lowercase();
        let terms: Vec<&str> = ql.split_whitespace().collect();
        if terms.is_empty() {
            return Vec::new();
        }
        let joined = terms.join(" ");

        // 每个文档只保留得分最高的块
        let mut best: HashMap<usize, (i32, usize)> = HashMap::new();
        for (ci, ch) in self.chunks.iter().enumerate() {
            let mut score = 0i32;
            let mut all = true;
            for t in &terms {
                match Self::count_at(&ch.lower, t, 3) {
                    Some((count, _pos)) => {
                        score += 10 * count;
                    }
                    None => {
                        all = false;
                        break;
                    }
                }
            }
            if !all {
                continue;
            }
            if ch.lower.contains(&joined) {
                score += 60;
            }
            let title_lower = self.docs[ch.doc].title.to_lowercase();
            if terms.iter().all(|t| title_lower.contains(t)) {
                score += 150;
                if title_lower.contains(&joined) {
                    score += 40;
                }
            }
            let e = best.entry(ch.doc).or_insert((score, ci));
            if score > e.0 {
                *e = (score, ci);
            }
        }

        let mut hits: Vec<Hit> = best
            .into_iter()
            .map(|(doc, (score, ci))| {
                let ch = &self.chunks[ci];
                Hit {
                    title: self.docs[doc].title.clone(),
                    path: self.docs[doc].path.clone(),
                    abs: self.docs[doc].abs.clone(),
                    snippet: make_snippet(&ch.text, &terms),
                    line: ch.line,
                    score,
                }
            })
            .collect();
        hits.sort_by(|a, b| b.score.cmp(&a.score).then_with(|| a.path.cmp(&b.path)));
        hits.truncate(50);
        hits
    }
}

/// 从原文裁剪命中片段：以首个命中词为中心取约 120 字符窗口，按字符边界对齐
fn make_snippet(text: &str, terms: &[&str]) -> String {
    let lower = text.to_lowercase();
    let pos = terms
        .iter()
        .filter_map(|t| lower.find(t))
        .min()
        .unwrap_or(0);
    // 将字节偏移吸附到字符边界
    let char_start = lower[..pos].chars().count();
    let chars: Vec<char> = text.chars().collect();
    const RADIUS: usize = 60;
    let start = char_start.saturating_sub(RADIUS);
    let end = (char_start + RADIUS).min(chars.len());
    let mut s = String::new();
    if start > 0 {
        s.push('…');
    }
    s.extend(&chars[start..end]);
    if end < chars.len() {
        s.push('…');
    }
    // 压缩换行便于列表展示
    s.replace('\n', " ")
}
