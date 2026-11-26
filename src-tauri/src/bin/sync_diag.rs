//! 同步诊断：以应用相同的配置与索引目录执行一次 sync_roots，打印真实结果/错误。
//! 用法：cargo run --bin sync_diag
use knowledge_dock_lib::{engine, scope};

fn main() {
    let appdata = std::env::var("APPDATA").expect("APPDATA");
    let base = std::path::PathBuf::from(appdata).join("com.local.knowledgedock");
    let cfg_json = std::fs::read_to_string(base.join("config.json")).expect("读 config.json");
    let cfg: knowledge_dock_lib::config::Config =
        knowledge_dock_lib::config::Config::from_json(&cfg_json).expect("解析/校验 config.json");
    println!(
        "roots(enabled)={:?}",
        cfg.effective_roots().iter().map(|r| r.path.clone()).collect::<Vec<_>>()
    );
    println!("scope={:?}", cfg.scope);

    let synonyms = engine::load_synonyms(Some(&base));
    let stopwords = engine::load_stopwords(Some(&base));
    let eng = engine::Engine::open_or_create(&base.join("index"), synonyms, stopwords)
        .expect("打开索引");
    let specs = cfg.root_specs();
    match eng.sync_roots(&specs, &cfg.scope) {
        Ok(stats) => println!("SYNC_OK docs={} chunks={}", stats.docs, stats.chunks),
        Err(e) => println!("SYNC_ERR: {e}"),
    }
    // 逐根扫描，定位是哪个根失败
    for spec in &specs {
        let opts = knowledge_dock_lib::indexer::PlanOptions {
            include_exts: if spec.exts.is_empty() { cfg.scope.include_exts.clone() } else { spec.exts.clone() },
            max_file_bytes: if spec.max_file_kb > 0 { spec.max_file_kb * 1024 } else { cfg.scope.max_file_kb * 1024 },
            exclude_globs: cfg.scope.exclude_globs.iter().cloned().chain(spec.excludes.iter().cloned()).collect(),
            include_globs: cfg.scope.include_globs.clone(),
            max_age_days: cfg.scope.max_age_days,
        };
        match knowledge_dock_lib::indexer::scan_index_plan(&spec.path, &opts) {
            Ok(ps) => println!("  root {} => {} files", spec.label, ps.len()),
            Err(e) => println!("  root {} => SCAN_ERR {e}", spec.label),
        }
    }
    let _ = scope::basename_of("x");
}
