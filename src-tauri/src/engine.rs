//! 检索引擎：Tantivy（BM25）+ jieba 中文分词 + 持久化索引 + 同义词查询改写
//!
//! - 索引落盘 app_data_dir/index，启动时 mmap 打开（秒开）
//! - notify 触发 sync：按文件 mtime 差异做增量 delete_by_term + add
//! - 查询：jieba 分词 → 同义词展开（静态词典）→ 标题加权 BM25

use crate::scope::ScopeConfig;
use crate::indexer::{self, DocumentChunk, Stats};
use jieba_rs::Jieba;
use serde::Serialize;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use tantivy::collector::{Count, TopDocs};
use tantivy::query::{AllQuery, BooleanQuery, BoostQuery, Occur, QueryParser, TermQuery};
use tantivy::schema::{
    Field, IndexRecordOption, NumericOptions, Schema, TextFieldIndexing, TextOptions, Value,
};
use tantivy::tokenizer::{Token, TokenStream, Tokenizer};
use tantivy::{doc, Index, IndexReader, IndexWriter, ReloadPolicy, TantivyDocument, Term};

/// 索引 schema 版本：字段/选项变更时递增（旧索引自动清空重建）
const SCHEMA_VERSION: u32 = 5;

/// 内嵌默认停用词（用户可通过配置目录 stopwords.json 覆盖扩展）
const DEFAULT_STOPWORDS: &str = include_str!("../assets/stopwords.json");

// ---------------- jieba 分词器接入 Tantivy ----------------

#[derive(Clone)]
pub struct JiebaTokenizer {
    jieba: Arc<Jieba>,
}

impl JiebaTokenizer {
    pub fn new() -> Self {
        JiebaTokenizer { jieba: Arc::new(Jieba::new()) }
    }
    /// 与查询侧共享同一分词实例（省一份词典内存）
    pub fn with_jieba(jieba: Arc<Jieba>) -> Self {
        JiebaTokenizer { jieba }
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
    /// 所属知识库根显示名
    pub root: String,
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
    #[serde(default)]
    pub duration_ms: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct StatsDetail {
    pub by_type: HashMap<String, u64>,
    pub by_root: HashMap<String, u64>,
    pub total_docs: u64,
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

/// 加载停用词（内嵌默认 + 用户覆盖）
pub fn load_stopwords(config_dir: Option<&Path>) -> HashSet<String> {
    let mut set: HashSet<String> = serde_json::from_str(DEFAULT_STOPWORDS)
        .unwrap_or_default();
    if let Some(dir) = config_dir {
        let p = dir.join("stopwords.json");
        if let Ok(s) = std::fs::read_to_string(&p) {
            if let Ok(user_list) = serde_json::from_str::<Vec<String>>(&s) {
                for w in user_list {
                    let w = w.trim().to_lowercase();
                    if !w.is_empty() { set.insert(w); }
                }
            }
        }
    }
    set
}

// ---------------- 引擎 ----------------

struct Fields {
    path: Field,
    rel: Field,
    title: Field,
    content: Field,
    breadcrumbs: Field,
    doc_type: Field,
    root: Field,
    line: Field,
}

/// 缓存的查询解析器（schema/分词器固定，无需每次按键重建）
struct Parsers {
    title: QueryParser,
    content: QueryParser,
    breadcrumbs: QueryParser,
}

pub struct Engine {
    index_dir: PathBuf,
    reader: IndexReader,
    writer: Mutex<IndexWriter>,
    jieba: Arc<Jieba>,
    parsers: Parsers,
    fields: Fields,
    /// abs -> [mtime_ms, chunk_count]
    manifest: Mutex<HashMap<String, [u64; 2]>>,
    synonyms: Mutex<HashMap<String, Vec<String>>>,
    stopwords: Mutex<HashSet<String>>,
}

impl Engine {
    /// 打开或创建持久化索引（秒开：仅 mmap 元数据，不重建）
    pub fn open_or_create(index_dir: &Path, synonyms: HashMap<String, Vec<String>>, stopwords_set: HashSet<String>) -> Result<Engine, String> {
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
        let doc_type = schema_builder.add_text_field(
            "doc_type",
            TextOptions::default()
                .set_stored()
                .set_indexing_options(
                    TextFieldIndexing::default()
                        .set_tokenizer("raw")
                        .set_index_option(IndexRecordOption::Basic),
                ),
        );
        // 所属根显示名（raw 可索引，支持按库过滤）
        let root = schema_builder.add_text_field(
            "root",
            TextOptions::default()
                .set_stored()
                .set_indexing_options(
                    TextFieldIndexing::default()
                        .set_tokenizer("raw")
                        .set_index_option(IndexRecordOption::Basic),
                ),
        );
        let line = schema_builder.add_u64_field("line", NumericOptions::default().set_stored());
        let schema = schema_builder.build();

        // schema 版本化：版本不符/损坏 → 清空目录重建（manifest 一并清除触发全量同步）
        let sentinel = index_dir.join(".kd-created");
        let expected = format!("schema-{SCHEMA_VERSION}");
        let index = if sentinel.exists()
            && std::fs::read_to_string(&sentinel)
                .map(|s| s.trim() == expected)
                .unwrap_or(false)
        {
            Index::open_in_dir(index_dir).map_err(|e| format!("打开索引失败: {e}"))?
        } else {
            let _ = std::fs::remove_dir_all(index_dir);
            std::fs::create_dir_all(index_dir).map_err(|e| e.to_string())?;
            let idx = Index::create_in_dir(index_dir, schema.clone())
                .map_err(|e| format!("创建索引失败: {e}"))?;
            std::fs::write(&sentinel, &expected).map_err(|e| e.to_string())?;
            idx
        };

        // 分词器注册（索引与查询共用同一 jieba 实例）
        let jieba = Arc::new(Jieba::new());
        index.tokenizers().register(TOKENIZER_NAME, JiebaTokenizer::with_jieba(jieba.clone()));

        let writer = index
            .writer(30_000_000) // 30MB 内存预算，个人库绰绰有余
            .map_err(|e| e.to_string())?;
        let reader = index
            .reader_builder()
            .reload_policy(ReloadPolicy::OnCommitWithDelay)
            .try_into()
            .map_err(|e: tantivy::TantivyError| e.to_string())?;

        // 缓存查询解析器（OR 语义；标题/面包屑/正文三字段）
        let parsers = Parsers {
            title: QueryParser::new(schema.clone(), vec![title], index.tokenizers().clone()),
            content: QueryParser::new(schema.clone(), vec![content], index.tokenizers().clone()),
            breadcrumbs: QueryParser::new(schema.clone(), vec![breadcrumbs], index.tokenizers().clone()),
        };

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
            jieba,
            parsers,
            fields: Fields { path, rel, title, content, breadcrumbs, doc_type, root, line },
            manifest: Mutex::new(manifest),
            synonyms: Mutex::new(synonyms),
            stopwords: Mutex::new(stopwords_set),
        })
    }

    pub fn reload_synonyms(&self, synonyms: HashMap<String, Vec<String>>) {
        *self.synonyms.lock().expect("syn lock") = synonyms;
    }

    /// 调试/统计：当前可见文档总数
    pub fn num_docs(&self) -> u64 {
        self.reader.searcher().num_docs()
    }

    /// 增量同步（多知识库根聚合）：只 stat 全部文件做 mtime 差异，
    /// **仅重读重切变更文件**（大库免全量重扫）；多根时展示路径带库名前缀
    /// roots: (路径, 显示名)；scope: 范围规则
    pub fn sync_roots(&self, roots: &[crate::scope::RootSpec], scope: &ScopeConfig) -> Result<Stats, String> {
        if roots.is_empty() {
            return Ok(Stats { docs: 0, chunks: 0 });
        }
        let multi = roots.len() > 1;
        let base_opts = indexer::PlanOptions {
            include_exts: scope.include_exts.clone(),
            max_file_bytes: if scope.max_file_kb == 0 {
                4 * 1024 * 1024
            } else {
                scope.max_file_kb * 1024
            },
            exclude_globs: scope.exclude_globs.clone(),
            include_globs: scope.include_globs.clone(),
            max_age_days: scope.max_age_days,
        };
        let mut plan: Vec<(indexer::FilePlan, String)> = Vec::new();
        for spec in roots {
            // 每库独立范围：排除规则与全局合并；扩展名/大小上限非空时覆盖全局
            let mut opts = base_opts.clone();
            opts.exclude_globs.extend(spec.excludes.iter().cloned());
            if !spec.exts.is_empty() {
                opts.include_exts = spec.exts.clone();
            }
            if spec.max_file_kb > 0 {
                opts.max_file_bytes = spec.max_file_kb * 1024;
            }
            let ps = indexer::scan_index_plan(&spec.path, &opts)?;
            for p in ps {
                let rel = if multi {
                    format!("{}/{}", spec.label, p.rel)
                } else {
                    p.rel
                };
                plan.push((indexer::FilePlan { rel, ..p }, spec.label.clone()));
            }
        }

        let manifest_path = self.index_dir.join("manifest.json");
        let mut manifest = self.manifest.lock().expect("manifest lock");
        let first_build = manifest.is_empty() && !manifest_path.exists();

        let mut writer = self.writer.lock().expect("writer lock");

        // 差异集合：mtime 变化/新文件 → touched；清单里有但磁盘没了 → removed
        let plan_abs: HashSet<&String> = plan.iter().map(|(p, _)| &p.abs).collect();
        let touched: Vec<(&indexer::FilePlan, &String)> = plan
            .iter()
            .filter(|(p, _)| manifest.get(&p.abs).map(|m| m[0]) != Some(p.mtime))
            .map(|(p, l)| (p, l))
            .collect();
        let removed: Vec<String> = manifest
            .keys()
            .filter(|a| !plan_abs.contains(a))
            .cloned()
            .collect();

        let mut chunk_counts: HashMap<String, u64> = HashMap::new();
        if first_build || !touched.is_empty() || !removed.is_empty() {
            // 删除：移除文件 + 变更文件的旧文档
            for abs in &removed {
                writer.delete_term(Term::from_field_text(self.fields.path, abs));
            }
            for (p, _) in &touched {
                writer.delete_term(Term::from_field_text(self.fields.path, &p.abs));
            }
            // 重建：仅变更文件重新读取切块
            for (p, label) in &touched {
                let chunks = indexer::chunk_file(Path::new(&p.abs), &p.rel);
                chunk_counts.insert(p.abs.clone(), chunks.len() as u64);
                for c in &chunks {
                    self.add_doc(&mut writer, c, label);
                }
            }
            writer.commit().map_err(|e| e.to_string())?;
            // 立即重载读取端：保证同步完成后即刻可搜（不等异步延迟重载）
            self.reader.reload().map_err(|e| e.to_string())?;
        }

        // 更新清单：未变更文件沿用原计数
        let mut new_manifest: HashMap<String, [u64; 2]> = HashMap::with_capacity(plan.len());
        for (p, _) in &plan {
            let entry = if chunk_counts.contains_key(&p.abs) {
                [p.mtime, chunk_counts[&p.abs]]
            } else {
                *manifest.get(&p.abs).unwrap_or(&[p.mtime, 0])
            };
            new_manifest.insert(p.abs.clone(), entry);
        }
        let stats = Stats {
            docs: new_manifest.len(),
            chunks: new_manifest.values().map(|m| m[1] as usize).sum(),
        };
        *manifest = new_manifest;
        let _ = std::fs::write(
            &manifest_path,
            serde_json::to_string(&*manifest).unwrap_or_else(|_| "{}".into()),
        );

        Ok(stats)
    }

    /// 当前生效的同义词词典（默认 + 用户覆盖合并后）
    pub fn synonyms_map(&self) -> HashMap<String, Vec<String>> {
        self.synonyms.lock().expect("syn lock").clone()
    }

    pub fn stopwords_set(&self) -> HashSet<String> {
        self.stopwords.lock().expect("sw lock").clone()
    }

    pub fn reload_stopwords(&self, sw: HashSet<String>) {
        *self.stopwords.lock().expect("sw lock") = sw;
    }

    fn add_doc(&self, writer: &mut IndexWriter, c: &DocumentChunk, root_label: &str) {
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
                f.root => root_label,
                f.line => c.line as u64,
            ))
            .ok();
    }

    pub fn stats(&self) -> Stats {
        let manifest = self.manifest.lock().expect("manifest lock");
        let chunks: u64 = manifest.values().map(|m| m[1]).sum();
        Stats { docs: manifest.len(), chunks: chunks as usize }
    }

    /// 索引统计仪表盘：各类型/各库块数分布
    pub fn stats_detail(&self) -> StatsDetail {
        let searcher = self.reader.searcher();
        let total = searcher.num_docs();

        let by_type: HashMap<String, u64> = ["Law", "Manual", "QA", "Note"]
            .iter()
            .map(|t| {
                let term = Term::from_field_text(self.fields.doc_type, t);
                let q = TermQuery::new(term, IndexRecordOption::Basic);
                let count = searcher.search(&q, &Count).unwrap_or(0) as u64;
                (t.to_string(), count)
            })
            .collect();

        let mut by_root: HashMap<String, u64> = HashMap::new();
        let all_q = AllQuery;
        let all_top = searcher
            .search(&all_q, &TopDocs::with_limit(100_000))
            .unwrap_or_default();
        for (_, addr) in all_top {
            if let Ok(d) = searcher.doc::<TantivyDocument>(addr) {
                if let Some(v) = d.get_first(self.fields.root) {
                    if let Some(s) = v.as_str() {
                        *by_root.entry(s.to_string()).or_insert(0) += 1;
                    }
                }
            }
        }

        StatsDetail { by_type, by_root, total_docs: total }
    }

    /// BM25 检索：分词 → 停用词 → 同义词展开 → 标题/面包屑加权
    /// type_filter/root_filter：范围限定（Some 时以 Must 子句下沉到引擎）
    pub fn search(
        &self,
        query: &str,
        limit: usize,
        type_filter: Option<&str>,
        root_filter: Option<&str>,
    ) -> SearchResponse {
        let q = query.trim();
        if q.is_empty() {
            return SearchResponse { hits: Vec::new(), terms: Vec::new(), duration_ms: 0 };
        }

        // 1. jieba 分词 + 词元清洗：仅保留字母数字（和数字上下文的 . -），剔除中英文标点
        let mut terms: Vec<String> = Vec::new();
        for t in self.jieba.cut(q, false) {
            let cleaned: String = t
                .chars()
                .filter(|c| c.is_alphanumeric() || *c == '.' || *c == '-')
                .collect();
            if !cleaned.chars().any(|c| c.is_alphanumeric()) {
                continue;
            }
            let lower = cleaned.to_lowercase();
            if !terms.contains(&lower) {
                terms.push(lower);
            }
        }
        // 2. 停用词过滤（若全部被滤掉则回退原始分词，避免把"怎么办"这类查询变成空查询）
        let filtered: Vec<String> = terms
            .iter()
            .map(|s| s.as_str())
            .filter(|t| !self.stopwords.lock().expect("sw lock").contains(*t))
            .map(|t| t.to_string())
            .collect();
        if !filtered.is_empty() {
            terms = filtered;
        }
        if terms.is_empty() {
            return SearchResponse { hits: Vec::new(), terms: Vec::new(), duration_ms: 0 };
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

        // 4. 构造查询：默认 OR 语义（同义词展开按用户设计为并集，BM25 负责排序）
        //    解析器已缓存，无需每次按键重建
        let title_q = build(&self.parsers.title, &effective);
        let content_q = build(&self.parsers.content, &effective);
        let bc_q = build(&self.parsers.breadcrumbs, &effective);
        let mut clauses: Vec<(Occur, Box<dyn tantivy::query::Query>)> = Vec::new();
        // 范围限定：Must 子句在候选集阶段即收窄（Top50 不会被其他范围的结果挤占）
        if let Some(t) = type_filter {
            let term = Term::from_field_text(self.fields.doc_type, t);
            clauses.push((
                Occur::Must,
                Box::new(TermQuery::new(term, IndexRecordOption::Basic)),
            ));
        }
        if let Some(r) = root_filter {
            let term = Term::from_field_text(self.fields.root, r);
            clauses.push((
                Occur::Must,
                Box::new(TermQuery::new(term, IndexRecordOption::Basic)),
            ));
        }
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
            return SearchResponse { hits: Vec::new(), terms: effective, duration_ms: 0 };
        }
        let query = BooleanQuery::new(clauses);

        // 5. 检索
        let searcher = self.reader.searcher();
        let top = match searcher.search(&query, &TopDocs::with_limit(limit)) {
            Ok(t) => t,
            Err(_) => return SearchResponse { hits: Vec::new(), terms: effective, duration_ms: 0 },
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
            let root = get(self.fields.root);
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
                root,
                snippet: make_snippet(&content, &effective),
                line,
                abs,
                score,
            });
        }
        SearchResponse { hits, terms: effective, duration_ms: 0 }
    }
}

/// 用同义词展开后的词表构造单字段 OR 查询（解析失败时逐词加引号兜底）
fn build(parser: &QueryParser, effective: &[String]) -> Option<Box<dyn tantivy::query::Query>> {
    let joined = effective
        .iter()
        .map(|t| t.replace(['\\', '"', '(', ')', '^', '~', ':'], ""))
        .filter(|t| !t.is_empty())
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
            let quoted = effective
                .iter()
                .map(|t| format!("\"{}\"", t.replace('"', "")))
                .collect::<Vec<_>>()
                .join(" ");
            parser.parse_query(&quoted).ok()
        }
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
