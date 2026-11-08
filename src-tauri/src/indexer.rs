//! 差异化切块：把非结构化文本解析为"带上下文层级的结构化知识单元"
//!
//! - 法规类：`第X条` 正则切分，遍历时跟踪 编/章/节 层级，面包屑直达答案
//! - Markdown：标题树（pulldown-cmark，正确处理代码围栏）作为块边界
//! - 问答类：`问:/答:`（或 `Q:/A:`）成对识别
//! - 兜底：纯文本按空行分段
//!
//! 检索与打分在 engine.rs（Tantivy BM25 + jieba 分词）。

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;
use std::sync::OnceLock;
use walkdir::WalkDir;

const EXTS: [&str; 3] = ["md", "markdown", "txt"];
const IGNORE_DIRS: [&str; 6] = [
    "node_modules", "target", "dist", "__pycache__", ".git", ".obsidian",
];
const MAX_FILE_BYTES: u64 = 4 * 1024 * 1024;

/// 法规条文行：第X条（可选前置空白）
static RE_ARTICLE: OnceLock<regex::Regex> = OnceLock::new();
/// 法规层级行：第X编/章/节（允许 Markdown 标记前缀，标题文本随后）
static RE_CHAPTER: OnceLock<regex::Regex> = OnceLock::new();
/// 问答行：问：/Q: 等
static RE_Q: OnceLock<regex::Regex> = OnceLock::new();
/// 问答行：答：/A: 等
static RE_A: OnceLock<regex::Regex> = OnceLock::new();

fn re_article() -> &'static regex::Regex {
    RE_ARTICLE.get_or_init(|| {
        regex::Regex::new(r"^\s*第[一二三四五六七八九十百千万零两]+条").unwrap()
    })
}
fn re_chapter() -> &'static regex::Regex {
    RE_CHAPTER.get_or_init(|| {
        regex::Regex::new(r"^\s*(?:#+\s*)?(第[一二三四五六七八九十百千万零两]+[章节编]\s*[^\n]*)$").unwrap()
    })
}
fn re_q() -> &'static regex::Regex {
    RE_Q.get_or_init(|| regex::Regex::new(r"^\s*(?:问|Q)[:：.]\s*(.*)$").unwrap())
}
fn re_a() -> &'static regex::Regex {
    RE_A.get_or_init(|| regex::Regex::new(r"^\s*(?:答|A)[:：.]\s*(.*)$").unwrap())
}

pub const TYPE_LAW: &str = "Law";
pub const TYPE_MANUAL: &str = "Manual";
pub const TYPE_QA: &str = "QA";
pub const TYPE_NOTE: &str = "Note";

/// 统一知识单元：三种格式收敛到同一结构
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DocumentChunk {
    /// "Law" | "Manual" | "QA" | "Note"
    pub doc_type: String,
    /// 面包屑导航，如 ["劳动法", "第四章 工作时间和休息休假", "第四十四条"]
    pub breadcrumbs: Vec<String>,
    /// 文档标题（首个 `# ` 标题，否则文件名）
    pub title: String,
    /// 展示用相对路径
    pub path: String,
    /// 绝对路径（打开/上下文展开用）
    pub abs: String,
    /// 实际内容（条文、题干+解析等）
    pub text: String,
    /// 块起始行号（1-based）
    pub line: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct Stats {
    pub docs: usize,
    pub chunks: usize,
}

fn is_ignored_dir(name: &str) -> bool {
    name.starts_with('.') || IGNORE_DIRS.contains(&name)
}

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

/// 问答对数量，>=2 视为题库/FAQ 文档
fn count_qa_pairs(content: &str) -> usize {
    let mut in_q = false;
    let mut pairs = 0;
    for line in content.lines() {
        if re_q().is_match(line) {
            if !in_q {
                pairs += 1;
            }
            in_q = true;
        } else if in_q && (line.trim().is_empty() || re_a().is_match(line)) {
            if line.trim().is_empty() {
                in_q = false;
            }
        }
    }
    pairs
}

/// 是否法规文档：条文标记足够密集
fn is_law(content: &str) -> bool {
    content.lines().filter(|l| re_article().is_match(l)).count() >= 2
}

/// 待索引文件清单（只 stat 不读内容，供增量同步做 mtime 差异）
#[derive(Debug, Clone)]
pub struct FilePlan {
    pub rel: String,
    pub abs: String,
    pub mtime: u64,
}

pub fn mtime_ms(p: &Path) -> u64 {
    p.metadata()
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// 扫描目录，产出合格文件的清单（不读取文件内容）
pub fn scan_index_plan(root: &Path) -> Result<Vec<FilePlan>, String> {
    if !root.is_dir() {
        return Err(format!("目录不存在: {}", root.display()));
    }
    let mut plans = Vec::new();
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
        let abs = entry.path().to_string_lossy().to_string();
        let rel = entry
            .path()
            .strip_prefix(root)
            .unwrap_or(entry.path())
            .to_string_lossy()
            .replace('\\', "/");
        let mtime = mtime_ms(entry.path());
        plans.push(FilePlan { rel, abs, mtime });
    }
    Ok(plans)
}

/// 单文件读取 + 分类切块（增量同步只对变更文件调用）
pub fn chunk_file(abs: &Path, rel: &str) -> Vec<DocumentChunk> {
    let Ok(raw) = std::fs::read(abs) else { return Vec::new() };
    let content = String::from_utf8_lossy(&raw).into_owned();
    let stem = abs
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_default();
    let title = extract_title(&content, &stem);
    let abs_str = abs.to_string_lossy().to_string();

    if is_law(&content) {
        chunk_law(&content, &title, rel, &abs_str)
    } else if count_qa_pairs(&content) >= 2 {
        chunk_qa(&content, &title, rel, &abs_str)
    } else if rel.to_lowercase().ends_with(".md") || rel.to_lowercase().ends_with(".markdown") {
        chunk_markdown(&content, &title, rel, &abs_str)
    } else {
        chunk_plain(&content, &title, rel, &abs_str)
    }
}

/// 全量扫描（测试/验证工具用）：清单 + 逐文件切块
pub fn scan_folder(root: &Path) -> Result<Vec<DocumentChunk>, String> {
    let mut all = Vec::new();
    for p in scan_index_plan(root)? {
        all.extend(chunk_file(Path::new(&p.abs), &p.rel));
    }
    Ok(all)
}

/// push 前统一修整：去首尾空行、压缩行尾空白
fn finalize(buf: &mut String) -> Option<String> {
    let text = buf.trim().to_string();
    buf.clear();
    if text.is_empty() {
        return None;
    }
    Some(text)
}

/// 法规：按"条"切分，跟踪 编/章/节；面包屑 = [文档标题, ..., "第X条"]
fn chunk_law(content: &str, title: &str, rel: &str, abs: &str) -> Vec<DocumentChunk> {
    let mut out = Vec::new();
    let mut stack: Vec<String> = Vec::new(); // 编/章/节 栈
    let mut buf = String::new();
    let mut start_line = 1usize;
    let mut cur_article: Option<String> = None;
    let mut ln = 1usize;

    for line in content.lines() {
        let t = line.trim_end();
        if let Some(c) = re_chapter().captures(t) {
            // 层级行：先收掉当前条
            if let Some(text) = finalize(&mut buf) {
                let mut bc: Vec<String> = vec![title.to_string()];
                bc.extend(stack.iter().cloned());
                if let Some(a) = &cur_article {
                    bc.push(a.clone());
                }
                out.push(chunk(TYPE_LAW, bc, title, rel, abs, text, start_line));
            }
            cur_article = None;
            let label = c[1].trim().to_string();
            // 同级或上级：弹出；下级：压栈（编>章>节 顺序）
            let level = |s: &str| match s.chars().find(|c| "编章节".contains(*c)) {
                Some('编') => 0,
                Some('章') => 1,
                _ => 2,
            };
            let lv = level(&label);
            stack.retain(|s| level(s) < lv);
            stack.push(label);
            ln += 1;
            continue;
        }
        if re_article().is_match(t) {
            // 新条：先收上一条
            if let Some(text) = finalize(&mut buf) {
                let mut bc: Vec<String> = vec![title.to_string()];
                bc.extend(stack.iter().cloned());
                if let Some(a) = &cur_article {
                    bc.push(a.clone());
                }
                out.push(chunk(TYPE_LAW, bc, title, rel, abs, text, start_line));
            }
            // 提取条号（如"第四十四条"）
            let m = re_article().find(t).unwrap();
            let article = m.as_str().trim().to_string();
            cur_article = Some(article);
            start_line = ln;
            buf.push_str(t);
            buf.push('\n');
            ln += 1;
            continue;
        }
        if !buf.is_empty() || !t.trim().is_empty() {
            if buf.is_empty() {
                start_line = ln;
            }
            buf.push_str(t);
            buf.push('\n');
        }
        ln += 1;
    }
    if let Some(text) = finalize(&mut buf) {
        let mut bc: Vec<String> = vec![title.to_string()];
        bc.extend(stack.iter().cloned());
        if let Some(a) = &cur_article {
            bc.push(a.clone());
        }
        out.push(chunk(TYPE_LAW, bc, title, rel, abs, text, start_line));
    }
    // 法规文件的普通前言（如目录说明）也保留
    out
}

/// 问答：问/答成对切块，面包屑 = [文档标题, "问N: 题干前 24 字"]
fn chunk_qa(content: &str, title: &str, rel: &str, abs: &str) -> Vec<DocumentChunk> {
    let mut out = Vec::new();
    let mut buf = String::new();
    let mut start_line = 1usize;
    let mut qno = 0usize;
    let mut qtext = String::new();
    let mut ln = 1usize;

    let flush = |buf: &mut String, out: &mut Vec<DocumentChunk>, start: usize, no: usize, q: &str| {
        if let Some(text) = finalize(buf) {
            let head: String = q.chars().take(24).collect();
            let label = if head.is_empty() { format!("问{no}") } else { format!("问{no}: {head}") };
            out.push(chunk(
                TYPE_QA,
                vec![title.to_string(), label],
                title,
                rel,
                abs,
                text,
                start,
            ));
        }
    };

    for line in content.lines() {
        let t = line.trim_end();
        if let Some(c) = re_q().captures(t) {
            flush(&mut buf, &mut out, start_line, qno, &qtext);
            qno += 1;
            qtext = c[1].trim().to_string();
            start_line = ln;
            buf.push_str(t);
            buf.push('\n');
        } else if !t.trim().is_empty() || (!buf.is_empty() && !re_a().is_match(t)) {
            if buf.is_empty() {
                start_line = ln;
            }
            buf.push_str(t);
            buf.push('\n');
        }
        ln += 1;
    }
    flush(&mut buf, &mut out, start_line, qno, &qtext);
    out
}

/// Markdown：标题树切块（pulldown-cmark，正确处理代码围栏/表格）
fn chunk_markdown(content: &str, title: &str, rel: &str, abs: &str) -> Vec<DocumentChunk> {
    use pulldown_cmark::{Event, Options, Parser, Tag, TagEnd};

    // 行首字节偏移表：byte -> line
    let mut line_starts = vec![0usize];
    for (i, b) in content.bytes().enumerate() {
        if b == b'\n' {
            line_starts.push(i + 1);
        }
    }

    let mut out = Vec::new();
    let mut stack: Vec<(usize, String)> = Vec::new(); // (level, 标题文本)
    let mut buf = String::new();
    let mut sec_start_byte = 0usize;
    let mut in_heading = false;

    for (ev, range) in Parser::new_ext(content, Options::ENABLE_TABLES).into_offset_iter() {
        match ev {
            Event::Start(Tag::Heading { level, .. }) => {
                flush_md(&mut buf, &mut out, &stack, title, rel, abs, sec_start_byte, &line_starts);
                // 弹出到父级并压入当前标题
                stack.retain(|(l, _)| *l < level as usize);
                stack.push((level as usize, String::new()));
                in_heading = true;
            }
            Event::End(TagEnd::Heading(_)) => {
                in_heading = false;
                // H1 与文档标题相同则不重复进入面包屑
                if let Some(last) = stack.last() {
                    if last.0 == 1 && last.1 == title {
                        stack.pop();
                    }
                }
                sec_start_byte = range.end;
            }
            Event::Text(t) => {
                if in_heading {
                    if let Some(last) = stack.last_mut() {
                        if !last.1.is_empty() {
                            last.1.push(' ');
                        }
                        last.1.push_str(t.trim());
                    }
                } else {
                    if buf.is_empty() {
                        sec_start_byte = range.start;
                    }
                    buf.push_str(&t);
                }
            }
            Event::Code(t) => {
                if buf.is_empty() {
                    sec_start_byte = range.start;
                }
                buf.push_str(&t);
            }
            Event::SoftBreak | Event::HardBreak => buf.push('\n'),
            Event::End(TagEnd::Paragraph)
            | Event::End(TagEnd::Table)
            | Event::End(TagEnd::List(false))
            | Event::End(TagEnd::BlockQuote(_))
            | Event::End(TagEnd::CodeBlock) => {
                flush_md(&mut buf, &mut out, &stack, title, rel, abs, sec_start_byte, &line_starts);
                sec_start_byte = range.end;
            }
            _ => {}
        }
    }
    flush_md(&mut buf, &mut out, &stack, title, rel, abs, sec_start_byte, &line_starts);
    out
}

/// 收尾一个 Markdown 段块：面包屑 = [文档标题, ...标题栈]
fn flush_md(
    buf: &mut String,
    out: &mut Vec<DocumentChunk>,
    stack: &[(usize, String)],
    title: &str,
    rel: &str,
    abs: &str,
    start_byte: usize,
    line_starts: &[usize],
) {
    if let Some(text) = finalize(buf) {
        let bc: Vec<String> = std::iter::once(title.to_string())
            .chain(stack.iter().map(|(_, s)| s.clone()))
            .collect();
        let line = match line_starts.binary_search(&start_byte) {
            Ok(i) => i + 1,
            Err(i) => i,
        };
        out.push(chunk(TYPE_MANUAL, bc, title, rel, abs, text, line));
    }
}

/// 纯文本兜底：按空行分段
fn chunk_plain(content: &str, title: &str, rel: &str, abs: &str) -> Vec<DocumentChunk> {
    let mut out = Vec::new();
    let mut buf = String::new();
    let mut start_line = 1usize;
    let mut ln = 1usize;
    for line in content.lines() {
        let t = line.trim_end();
        if t.trim().is_empty() {
            if let Some(text) = finalize(&mut buf) {
                out.push(chunk(TYPE_NOTE, vec![title.to_string()], title, rel, abs, text, start_line));
            }
        } else {
            if buf.is_empty() {
                start_line = ln;
            }
            buf.push_str(t);
            buf.push('\n');
        }
        ln += 1;
    }
    if let Some(text) = finalize(&mut buf) {
        out.push(chunk(TYPE_NOTE, vec![title.to_string()], title, rel, abs, text, start_line));
    }
    out
}

#[allow(clippy::too_many_arguments)]
fn chunk(
    doc_type: &str,
    breadcrumbs: Vec<String>,
    title: &str,
    rel: &str,
    abs: &str,
    text: String,
    line: usize,
) -> DocumentChunk {
    DocumentChunk {
        doc_type: doc_type.to_string(),
        breadcrumbs,
        title: title.to_string(),
        path: rel.to_string(),
        abs: abs.to_string(),
        text,
        line,
    }
}

/// 供测试与工具核对面包屑组装
pub fn law_breadcrumb(title: &str, stack: &[String], article: &str) -> Vec<String> {
    let mut bc = vec![title.to_string()];
    bc.extend(stack.iter().cloned());
    bc.push(article.to_string());
    bc
}

/// (调试用) 统计各类型块数
pub fn stats_by_type(chunks: &[DocumentChunk]) -> HashMap<&str, usize> {
    let mut m: HashMap<&str, usize> = HashMap::new();
    for c in chunks {
        *m.entry(match c.doc_type.as_str() {
            TYPE_LAW => "Law",
            TYPE_MANUAL => "Manual",
            TYPE_QA => "QA",
            _ => "Note",
        })
        .or_insert(0) += 1;
    }
    m
}
