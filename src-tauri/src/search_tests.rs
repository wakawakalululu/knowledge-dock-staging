//! 搜索引擎测试：切块结构 + Tantivy BM25 + 同义词展开
//! 全部使用临时目录自建样本，CI 环境可直接运行

#[cfg(test)]
mod search_tests {
    use crate::scope::ScopeConfig;
    use crate::engine::{load_stopwords, load_synonyms, Engine};
    use crate::indexer;
    use std::fs;
    use std::path::PathBuf;

    fn spec(dir: &std::path::Path, label: &str) -> crate::scope::RootSpec {
        crate::scope::RootSpec {
            path: dir.to_path_buf(),
            label: label.to_string(),
            excludes: Vec::new(),
            exts: Vec::new(),
            max_file_kb: 0,
        }
    }

    fn temp_dir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("kd-test-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    fn write(path: &PathBuf, content: &str) {
        fs::write(path, content).unwrap();
    }

    const LAW: &str = "# 中华人民共和国劳动法（示例节选）\n\n## 第四章 工作时间和休息休假\n\n第三十六条 国家实行劳动者每日工作时间不超过八小时。\n\n第四十四条 用人单位应当支付高于正常工作时间工资的报酬，安排延长工作时间的支付加班费。\n\n## 第十二章 法律责任\n\n第九十条 违反本法规定延长劳动者工作时间的，责令改正并处罚款。\n";

    const QA: &str = "# 员工手册问答\n\n问：离职需要提前多久提出申请？\n答：正式员工需提前三十日以书面形式通知用人单位。\n\n问：被解雇了怎么申请经济补偿？\n答：按工作年限每满一年支付一个月工资的标准支付补偿金。\n";

    #[test]
    fn law_chunking_binds_chapter_and_article() {
        let chunks = chunk_str(LAW);
        let art44 = chunks
            .iter()
            .find(|c| c.breadcrumbs.last().map(|s| s.contains("第四十四条")).unwrap_or(false))
            .expect("应切出第四十四条");
        assert_eq!(art44.doc_type, "Law");
        assert!(art44.breadcrumbs.iter().any(|s| s.contains("第四章")), "面包屑应绑定章节: {:?}", art44.breadcrumbs);
        assert!(art44.text.contains("加班费"));
        assert!(art44.line > 1, "应记录行号");
    }

    fn chunk_str(content: &str) -> Vec<indexer::DocumentChunk> {
        // 写入临时文件，走 scan_index_plan + chunk_file 完整管线
        let dir = temp_dir("chunk");
        write(&dir.join("test.md"), content);
        let opts = indexer::PlanOptions {
            include_exts: Vec::new(),
            max_file_bytes: 0,
            exclude_globs: Vec::new(),
            include_globs: Vec::new(),
            max_age_days: 0,
        };
        let mut out = Vec::new();
        for p in indexer::scan_index_plan(&dir, &opts).unwrap() {
            out.extend(indexer::chunk_file(std::path::Path::new(&p.abs), &p.rel));
        }
        out
    }

    #[test]
    fn qa_chunking_pairs_question_answer() {
        let chunks = chunk_str(QA);
        assert!(chunks.iter().all(|c| c.doc_type == "QA"), "应识别为 QA: {:?}", chunks.iter().map(|c| c.doc_type.clone()).collect::<Vec<_>>());
        assert!(chunks.iter().any(|c| c.text.contains("解雇") && c.text.contains("补偿")));
        assert!(chunks.iter().all(|c| c.breadcrumbs.len() == 2));
    }

    #[test]
    fn bm25_ranks_law_article_first_with_breadcrumbs() {
        let root = temp_dir("engine-law");
        write(&root.join("劳动法.md"), LAW);
        write(&root.join("noise.md"), "# 笔记\n\n随便写点 2024 年的杂事，包含劳动法三个字但与加班无关。\n");
        let engine = Engine::open_or_create(&temp_dir("idx-law"), load_synonyms(None), load_stopwords(None)).unwrap();
        let stats = engine.sync_roots(&[spec(&root, "test")], &ScopeConfig::default()).unwrap();
        assert!(stats.docs >= 2 && stats.chunks >= 4, "{stats:?}");

        let r = engine.search("加班费", 10, None, None);
        assert!(!r.hits.is_empty());
        let top = &r.hits[0];
        assert!(top.breadcrumbs.iter().any(|s| s.contains("第四十四条")), "BM25 应把真正的答案条文顶到第一: {:?} score={}", top.breadcrumbs, top.score);
        assert_eq!(top.doc_type, "Law");
    }

    #[test]
    fn synonym_expansion_recalls_layoff_answer() {
        let root = temp_dir("engine-syn");
        write(&root.join("qa.md"), QA);
        let engine = Engine::open_or_create(&temp_dir("idx-syn"), load_synonyms(None), load_stopwords(None)).unwrap();
        engine.sync_roots(&[spec(&root, "test")], &ScopeConfig::default()).unwrap();

        // “辞职”不在文档中，但同义词展开应命中“离职/解雇”
        let r = engine.search("辞职", 10, None, None);
        assert!(!r.hits.is_empty(), "同义词展开应召回离职/解雇问答");
        assert!(r.terms.iter().any(|t| t == "离职" || t == "解雇"), "terms 应含展开词: {:?}", r.terms);
        let joined: String = r.hits.iter().map(|h| h.snippet.clone()).collect();
        assert!(joined.contains("离职") || joined.contains("解雇"));
    }

    #[test]
    fn incremental_update_picks_up_changes() {
        let root = temp_dir("engine-inc");
        write(&root.join("a.md"), "# A\n\n最初的关于公积金的内容。\n");
        let engine = Engine::open_or_create(&temp_dir("idx-inc"), load_synonyms(None), load_stopwords(None)).unwrap();
        engine.sync_roots(&[spec(&root, "test")], &ScopeConfig::default()).unwrap();
        assert!(engine.search("公积金", 10, None, None).hits.is_empty() == false);

        // 修改文件：旧词消失、新词出现
        std::thread::sleep(std::time::Duration::from_millis(20)); // 确保 mtime 变化
        write(&root.join("a.md"), "# A\n\n改写后的年金计划说明。\n");
        let stats = engine.sync_roots(&[spec(&root, "test")], &ScopeConfig::default()).unwrap();
        assert_eq!(stats.docs, 1);
        assert!(engine.search("公积金", 10, None, None).hits.is_empty(), "旧内容应被增量删除");
        assert!(!engine.search("年金", 10, None, None).hits.is_empty(), "新内容应被增量加入");
    }

    #[test]
    fn multi_root_sync_aggregates_and_prefixes_rel() {
        let r1 = temp_dir("multi-a");
        let r2 = temp_dir("multi-b");
        write(&r1.join("劳动法.md"), LAW);
        write(&r2.join("手册.md"), "# 手册\n\n关于辞退赔偿的说明。\n");
        let engine = Engine::open_or_create(&temp_dir("idx-multi"), load_synonyms(None), load_stopwords(None)).unwrap();
        let stats = engine.sync_roots(&[spec(&r1, "multi-a"), spec(&r2, "multi-b")], &ScopeConfig::default()).unwrap();
        assert_eq!(stats.docs, 2, "两个根各 1 个文档: {stats:?}");

        let r = engine.search("赔偿", 10, None, None);
        assert!(!r.hits.is_empty(), "跨根检索应命中");
        assert!(
            r.hits.iter().any(|h| h.path.starts_with("multi-a/") || h.path.starts_with("multi-b/")),
            "多根时展示路径应带库名前缀: {:?}",
            r.hits.iter().map(|h| &h.path).collect::<Vec<_>>()
        );
    }
}
