//! 搜索引擎测试：直接对 sample-notes 真实数据验证扫描与检索
//! （作为 lib 内单元测试运行，避免集成测试 exe 在 GNU 工具链下的 DLL 加载问题）
//! sample-notes 仅存在于本机开发工作区；CI 上自动跳过。

#[cfg(test)]
mod search_tests {
    use crate::indexer::Index;
    use std::path::Path;

    fn with_sample(f: impl FnOnce(&Index)) {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../sample-notes");
        if !root.is_dir() {
            eprintln!("skip: sample-notes 不存在（非本机开发环境）");
            return;
        }
        let idx = Index::build(&root).expect("构建索引失败");
        f(&idx);
    }

    #[test]
    fn scan_finds_all_docs() {
        with_sample(|idx| {
            let s = idx.stats();
            assert_eq!(s.docs, 4, "应扫描到 4 个 md 文档: {s:?}");
            assert!(s.chunks >= 4, "分块数应 >= 4: {s:?}");
        });
    }

    #[test]
    fn chinese_substring_search() {
        with_sample(|idx| {
            let hits = idx.search("部署");
            assert!(!hits.is_empty(), "『部署』应有命中");
            let docker = hits
                .iter()
                .find(|h| h.path.contains("Docker"))
                .expect("应命中 Docker 部署手册");
            assert!(docker.snippet.contains("部署"), "片段应包含命中词: {}", docker.snippet);
        });
    }

    #[test]
    fn multi_term_search() {
        with_sample(|idx| {
            let hits = idx.search("docker 镜像");
            assert!(!hits.is_empty());
            assert!(
                hits[0].path.contains("Docker"),
                "多词全含应命中 Docker 手册，实际: {}",
                hits[0].path
            );
        });
    }

    #[test]
    fn title_match_ranks_first() {
        with_sample(|idx| {
            let hits = idx.search("git");
            assert!(!hits.is_empty());
            assert_eq!(
                hits[0].path, "Git常用操作.md",
                "标题命中应排第一: {:?}",
                hits.iter().map(|h| &h.path).collect::<Vec<_>>()
            );
        });
    }

    #[test]
    fn no_result_for_garbage() {
        with_sample(|idx| {
            assert!(idx.search("zzzz不存在的词").is_empty());
        });
    }

    #[test]
    fn empty_query_is_empty() {
        with_sample(|idx| {
            assert!(idx.search("").is_empty());
            assert!(idx.search("   ").is_empty());
        });
    }

    #[test]
    fn snippet_contains_hit() {
        with_sample(|idx| {
            let hits = idx.search("所有权");
            assert!(!hits.is_empty(), "『所有权』应命中 Rust 笔记");
            let h = hits
                .iter()
                .find(|h| h.path.contains("Rust"))
                .expect("Rust 笔记应命中");
            assert!(h.snippet.contains("所有权"), "片段应包含命中词: {}", h.snippet);
        });
    }
}
