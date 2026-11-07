//! 检索引擎：Tantivy（BM25）+ jieba 中文分词 + 持久化索引 + 同义词查询改写
//!
//! - 索引落盘 app_data_dir/index，启动时 mmap 打开（秒开）
//! - notify 触发 sync：按文件 mtime 差异做增量 delete_by_term + add
//! - 查询：jieba 分词 → 同义词展开（静态词典）→ 标题加权 BM25

use crate::indexer::{self, DocumentChunk, Stats};
use jieba_rs::Jieba;
use serde::Serialize;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use tantivy::collector::TopDocs;
use tantivy::query::{BooleanQuery, BoostQuery, Occur, QueryParser};
use tantivy::schema::{
    Field, IndexRecordOption, NumericOptions, Schema, TextFieldIndexing, TextOptions, Value,
};
use tantivy::tokenizer::{Token, TokenStream, Tokenizer, TokenizerManager};
use tantivy::{doc, Index, IndexReader, IndexWriter, ReloadPolicy, TantivyDocument, Term};

// ---------------- jieba 分词器接入 Tantivy ----------------

#[derive(Clone)]
pub struct JiebaTokenizer {
    jieba: Arc<Jieba>,
}

impl JiebaTokenizer {
    pub fn new() -> Self {
        JiebaTokenizer { jieba: Arc::new(Jieba::new()) }
    }
}

impl Default for JiebaTokenizer {
    fn default() -> Self {
        Self::new()
    }
}

pub const TOKENIZER_NAME: &str = "jieba";

impl Tokenizer for JiebaTokenizer {
    type TokenStream<'a> = JiebaTokenStream;

    fn token_stream<'a>(&'a mut self, text: &'a str) -> Self::TokenStream<'a> {
        let mut tokens: Vec<Token> = Vec::new();
        // 精确模式（非重叠），顺序推进偏移；空段/标点由 jieba 保留原样，跳过纯空白
        let mut cursor = 0usize;
        for piece in self.jieba.cut(text, false) {
            let start = text[cursor..].find(piece).map(|p| cursor + p).unwrap_or(cursor);
            cursor = start + piece.len();
            if piece.trim().is_empty() {
                continue;
            }
            tokens.push(Token {
                offset_from: start,
                offset_to: cursor,
                position: tokens.len(),
                text: piece.to_lowercase(),
                position_length: 1,
            });
        }
        JiebaTokenStream { tokens, idx: 0 }
    }
}

pub struct JiebaTokenStream {
    tokens: Vec<Token>,
    idx: usize,
}

impl TokenStream for JiebaTokenStream {
    fn advance(&mut self) -> bool {
        if self.idx < self.tokens.len() {
            self.idx += 1;
            true
        } else {
            false
        }
    }
    fn token(&self) -> &Token {
        &self.tokens[self.idx - 1]
    }
    fn token_mut(&mut self) -> &mut Token {
        &mut self.tokens[self.idx - 1]
    }
}

// ---------------- 检索结果 ----------------

#[derive(Debug, Clone, Serialize)]
pub struct Hit {
    /// 展示用相对路径
    pub path: String,
    /// 文档标题
    pub title: String,
    /// 面包屑导航，如 [劳动法, 第四章..., 第四十四条]
    pub breadcrumbs: Vec<String>,
    /// "Law" | "Manual" | "QA" | "Note"
    pub doc_type: String,
    /// 命中片段
    pub snippet: String,
    /// 块起始行号
    pub line: usize,
    /// 绝对路径（打开 / 上下文展开）
    pub abs: String,
    #[serde(skip_serializing)]
    pub score: f32,
}

#[derive(Debug, Clone, Serialize)]
pub struct SearchResponse {
    pub hits: Vec<Hit>,
    /// 实际参与匹配的词（含同义词展开），供前端高亮
    pub terms: Vec<String>,
}

// ---------------- 同义词典 ----------------

const DEFAULT_SYNONYMS: &str = include_str!("../assets/synonyms.json");

/// 加载默认词典（内嵌）+ 用户覆盖（app_config_dir/synonyms.json），并做双向对称
pub fn load_synonyms(config_dir: Option<&Path>) -> HashMap<String, Vec<String>> {
    let mut dict: HashMap<String, Vec<String>> = HashMap::new();
    if let Ok(v) = serde_json::from_str::<HashMap<String, Vec<String>>>(DEFAULT_SYNONYMS) {
        for (k, vs) in v {
            dict.entry(k).or_default().extend(vs);
        }
    }
    if let Some(dir) = config_dir {
        let p = dir.join("synonyms.json");
        if let Ok(s) = std::fs::read_to_string(&p) {
            if let Ok(v) = serde_json::from_str::<HashMap<String, Vec<String>>>(&s) {
                for (k, vs) in v {
                    dict.entry(k).or_default().extend(vs);
                }
            }
        }
    }
    // 双向对称：a→b 亦 b→a
    let extra: Vec<(String, Vec<String>)> = dict
        .iter()
        .flat_map(|(k, vs)| vs.iter().map(move |v| (v.clone(), vec![k.clone()])))
        .collect();
    for (k, vs) in extra {
        dict.entry(k).or_default().extend(vs);
    }
    dict
}

// ---------------- 引擎 ----------------

struct Fields {
    path: Field,
    rel: Field,
    title: Field,
    content: Field,
    breadcrumbs: Field,
    doc_type: Field,
    line: Field,
}

pub struct Engine {
    index_dir: PathBuf,
    reader: IndexReader,
    writer: Mutex<IndexWriter>,
    tokenizers: TokenizerManager,
    fields: Fields,
    /// abs -> [mtime_ms, chunk_count]
    manifest: Mutex<HashMap<String, [u64; 2]>>,
    synonyms: Mutex<HashMap<String, Vec<String>>>,
}

fn mtime_ms(p: &Path) -> u64 {
    p.metadata()
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

impl Engine {
    /// 打开或创建持久化索引（秒开：仅 mmap 元数据，不重建）
    pub fn open_or_create(index_dir: &Path, synonyms: HashMap<String, Vec<String>>) -> Result<Engine, String> {
        std::fs::create_dir_all(index_dir).map_err(|e| e.to_string())?;
        let mut schema_builder = Schema::builder();

        // path：raw 分词（整串匹配），供增量删除
        let path_opts = TextOptions::default()
            .set_stored()
            .set_indexing_options(
                TextFieldIndexing::default()
                    .set_tokenizer("raw")
                    .set_index_option(IndexRecordOption::Basic),
            );
        let path = schema_builder.add_text_field("path", path_opts);
        // rel：仅存储，展示用相对路径
        let rel = schema_builder.add_text_field("rel", TextOptions::default().set_stored());
        let title = schema_builder.add_text_field(
            "title",
            TextOptions::default()
                .set_stored()
                .set_indexing_options(
                    TextFieldIndexing::default()
                        .set_tokenizer(TOKENIZER_NAME)
                        .set_index_option(IndexRecordOption::WithFreqsAndPositions),
                ),
        );
        let content = schema_builder.add_text_field(
            "content",
            TextOptions::default()
                .set_stored()
                .set_indexing_options(
                    TextFieldIndexing::default()
                        .set_tokenizer(TOKENIZER_NAME)
                        .set_fieldnorms(true)
                        .set_index_option(IndexRecordOption::WithFreqsAndPositions),
                ),
        );
        let breadcrumbs = schema_builder.add_text_field(
            "breadcrumbs",
            TextOptions::default()
                .set_stored()
                .set_indexing_options(
                    TextFieldIndexing::default()
                        .set_tokenizer(TOKENIZER_NAME)
                        .set_index_option(IndexRecordOption::WithFreqsAndPositions),
                ),
        );
        let doc_type = schema_builder.add_text_field("doc_type", TextOptions::default().set_stored());
        let line = schema_builder.add_u64_field("line", NumericOptions::default().set_stored());
        let schema = schema_builder.build();

        // 哨兵文件区分"空目录"与"已建索引"（不能用 meta.json —— 那是 tantivy 自己的元数据）
        let sentinel = index_dir.join(".kd-created");
        let index = if sentinel.exists() {
            Index::open_in_dir(index_dir).map_err(|e| format!("打开索引失败: {e}"))?
        } else {
            let idx = Index::create_in_dir(index_dir, schema.clone())
                .map_err(|e| format!("创建索引失败: {e}"))?;
            std::fs::write(&sentinel, "created").map_err(|e| e.to_string())?;
            idx
        };

        // 分词器注册（索引与查询共用）
        index.tokenizers().register(TOKENIZER_NAME, JiebaTokenizer::new());

        let writer = index
            .writer(30_000_000) // 30MB 内存预算，个人库绰绰有余
            .map_err(|e| e.to_string())?;
        let reader = index
            .reader_builder()
            .reload_policy(ReloadPolicy::OnCommitWithDelay)
            .try_into()
            .map_err(|e: tantivy::TantivyError| e.to_string())?;

        // 恢复清单（损坏则视为空，触发全量重建）
        let manifest_path = index_dir.join("manifest.json");
        let manifest: HashMap<String, [u64; 2]> = std::fs::read_to_string(&manifest_path)
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default();

        Ok(Engine {
            index_dir: index_dir.to_path_buf(),
            reader,
            writer: Mutex::new(writer),
            tokenizers: index.tokenizers().clone(),
            fields: Fields { path, rel, title, content, breadcrumbs, doc_type, line },
            manifest: Mutex::new(manifest),
            synonyms: Mutex::new(synonyms),
        })
    }

    pub fn reload_synonyms(&self, synonyms: HashMap<String, Vec<String>>) {
        *self.synonyms.lock().expect("syn lock") = synonyms;
    }

    /// 调试/统计：当前可见文档总数
    pub fn num_docs(&self) -> u64 {
        self.reader.searcher().num_docs()
    }

    /// 按文件 mtime 差异做增量同步：删除失效/变更文件的旧文档，重建变更文件
    pub fn sync(&self, root: &Path) -> Result<Stats, String> {
        let chunks = indexer::scan_folder(root)?;
        let mut current: HashMap<String, (u64, usize)> = HashMap::new(); // abs -> (mtime, chunk_count)
        for c in &chunks {
            let e = current.entry(c.abs.clone()).or_insert((0, 0));
            e.0 = mtime_ms(Path::new(&c.abs));
            e.1 += 1;
        }

        let manifest_path = self.index_dir.join("manifest.json");
        let mut manifest = self.manifest.lock().expect("manifest lock");
        let first_build = manifest.is_empty() && !manifest_path.exists();

        let mut by_abs: HashMap<&str, Vec<&DocumentChunk>> = HashMap::new();
        for c in &chunks {
            by_abs.entry(c.abs.as_str()).or_default().push(c);
        }

        let mut writer = self.writer.lock().expect("writer lock");
        // 变更/删除集合：mtime 变化、清单缺失、文件被移除
        let mut touched: HashSet<String> = HashSet::new();
        for (abs, (mtime, _)) in &current {
            if manifest.get(abs).map(|m| m[0]) != Some(*mtime) {
                touched.insert(abs.clone());
            }
        }
        for abs in manifest.keys() {
            if !current.contains_key(abs) {
                touched.insert(abs.clone());
            }
        }

        if first_build || !touched.is_empty() {
            // 逐文件删除旧文档
            for abs in &touched {
                let term = Term::from_field_text(self.fields.path, abs);
                writer.delete_term(term);
            }
            // 重建变更文件
            for abs in &touched {
                if let Some(list) = by_abs.get(abs.as_str()) {
                    for c in list {
                        self.add_doc(&mut writer, c);
                    }
                }
            }
            writer.commit().map_err(|e| e.to_string())?;
            // 立即重载读取端：保证同步完成后即刻可搜（不等异步延迟重载）
            self.reader.reload().map_err(|e| e.to_string())?;
        }

        *manifest = current
            .iter()
            .map(|(k, (m, n))| (k.clone(), [*m, *n as u64]))
            .collect();
        let _ = std::fs::write(
            &manifest_path,
            serde_json::to_string(&*manifest).unwrap_or_else(|_| "{}".into()),
        );

        Ok(Stats { docs: manifest.len(), chunks: chunks.len() })
    }

    fn add_doc(&self, writer: &mut IndexWriter, c: &DocumentChunk) {
        let f = &self.fields;
        let bc = c.breadcrumbs.join(" > ");
        writer
            .add_document(doc!(
                f.path => c.abs.as_str(),
                f.rel => c.path.as_str(),
                f.title => c.title.as_str(),
                f.content => c.text.as_str(),
                f.breadcrumbs => bc.as_str(),
                f.doc_type => c.doc_type.as_str(),
                f.line => c.line as u64,
            ))
            .ok();
    }

    pub fn stats(&self) -> Stats {
        let manifest = self.manifest.lock().expect("manifest lock");
        let chunks: u64 = manifest.values().map(|m| m[1]).sum();
        Stats { docs: manifest.len(), chunks: chunks as usize }
    }

    /// BM25 检索：分词 → 同义词展开 → 标题加权
    pub fn search(&self, query: &str, limit: usize) -> SearchResponse {
        let q = query.trim();
        if q.is_empty() {
            return SearchResponse { hits: Vec::new(), terms: Vec::new() };
        }

        // 1. jieba 分词（复用 tokenizer 内部 jieba：新建轻量实例亦可，词典载入一次成本可忽略）
        static JIEBA: OnceLock<Jieba> = OnceLock::new();
        let jieba = JIEBA.get_or_init(Jieba::new);
        let mut terms: Vec<String> = Vec::new();
        for t in jieba.cut(q, false) {
            let t = t.trim();
            if t.is_empty() {
                continue;
            }
            let cleaned: String = t
                .chars()
                .filter(|c| c.is_alphanumeric() || !c.is_ascii_punctuation())
                .collect();
            if cleaned.is_empty() {
                continue;
            }
            let lower = cleaned.to_lowercase();
            if !terms.contains(&lower) {
                terms.push(lower);
            }
        }
        if terms.is_empty() {
            return SearchResponse { hits: Vec::new(), terms: Vec::new() };
        }

        // 2. 同义词展开（零网络、零模型，纯静态词典）
        //    除分词结果外，原始整串查询也参与词典查找（口语词可能被 jieba 拆散）
        let dict = self.synonyms.lock().expect("syn lock");
        let ql_raw = q.to_lowercase();
        let mut effective: Vec<String> = Vec::new();
        let mut lookup_keys: Vec<String> = terms.clone();
        if !lookup_keys.contains(&ql_raw) {
            lookup_keys.push(ql_raw);
        }
        for t in &lookup_keys {
            if !effective.contains(t) {
                effective.push(t.clone());
            }
            if let Some(vs) = dict.get(t.as_str()) {
                for v in vs {
                    let vl = v.to_lowercase();
                    if !effective.contains(&vl) {
                        effective.push(vl);
                    }
                }
            }
        }
        drop(dict);

        // 3. 构造查询：默认 OR 语义（同义词展开按用户设计为并集，BM25 负责排序）
        let searcher = self.reader.searcher();
        let schema = searcher.schema().clone();
        let title_parser =
            QueryParser::new(schema.clone(), vec![self.fields.title], self.tokenizers.clone());
        let content_parser =
            QueryParser::new(schema.clone(), vec![self.fields.content], self.tokenizers.clone());
        // 面包屑可搜：章节/条号（如"第四十四条"）直达答案
        let bc_parser =
            QueryParser::new(schema.clone(), vec![self.fields.breadcrumbs], self.tokenizers.clone());

        let build = |parser: &QueryParser| -> Option<Box<dyn tantivy::query::Query>> {
            let joined = effective
                .iter()
                .map(|t| t.replace(['\\', '"', '(', ')', '^', '~', ':'], ""))
                .filter(|t| !t.is_empty())
                .map(|t| format!("{t}"))
                .collect::<Vec<_>>()
                .join(" ");
            if joined.is_empty() {
                return None;
            }
            match parser.parse_query(&joined) {
                Ok(q) => Some(q),
                Err(e) => {
                    #[cfg(debug_assertions)]
                    eprintln!("[engine] parse_query 失败({e})，尝试加引号兜底: {joined}");
                    // 兜底：逐词加引号
                    let quoted = effective
                        .iter()
                        .map(|t| format!("\"{}\"", t.replace('"', "")))
                        .collect::<Vec<_>>()
                        .join(" ");
                    parser.parse_query(&quoted).ok()
                }
            }
        };

        let title_q = build(&title_parser);
        let content_q = build(&content_parser);
        let bc_q = build(&bc_parser);
        let mut clauses: Vec<(Occur, Box<dyn tantivy::query::Query>)> = Vec::new();
        if let Some(q) = title_q {
            clauses.push((Occur::Should, Box::new(BoostQuery::new(q, 2.0))));
        }
        if let Some(q) = bc_q {
            clauses.push((Occur::Should, Box::new(BoostQuery::new(q, 1.5))));
        }
        if let Some(q) = content_q {
            clauses.push((Occur::Should, q));
        }
        if clauses.is_empty() {
            return SearchResponse { hits: Vec::new(), terms: effective };
        }
        let query = BooleanQuery::new(clauses);

        // 4. 检索
        let top = match searcher.search(&query, &TopDocs::with_limit(limit)) {
            Ok(t) => t,
            Err(_) => return SearchResponse { hits: Vec::new(), terms: effective },
        };

        let mut hits = Vec::new();
        for (score, addr) in top {
            let Ok(d) = searcher.doc::<TantivyDocument>(addr) else { continue };
            let get = |f: Field| -> String {
                d.get_first(f)
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string()
            };
            let abs = get(self.fields.path);
            let rel = get(self.fields.rel);
            let title = get(self.fields.title);
            let content = get(self.fields.content);
            let breadcrumbs = get(self.fields.breadcrumbs);
            let doc_type = get(self.fields.doc_type);
            let line = d
                .get_first(self.fields.line)
                .and_then(|v| v.as_u64())
                .unwrap_or(0) as usize;
            let bc: Vec<String> = if breadcrumbs.is_empty() {
                Vec::new()
            } else {
                breadcrumbs.split(" > ").map(|s| s.to_string()).collect()
            };
            hits.push(Hit {
                path: rel,
                title,
                breadcrumbs: bc,
                doc_type,
                snippet: make_snippet(&content, &effective),                line,
                abs,
                score,
            });
        }
        SearchResponse { hits, terms: effective }
    }
}

/// 命中片段：以首个命中词为中心取约 120 字符窗口，按字符边界对齐
pub fn make_snippet(text: &str, terms: &[String]) -> String {
    let lower = text.to_lowercase();
    let pos = terms.iter().filter_map(|t| lower.find(t)).min().unwrap_or(0);
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
    s.replace('\n', " ")
}
