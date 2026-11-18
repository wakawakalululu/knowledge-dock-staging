//! 配置差异诊断：给定两份 config JSON，打印 dry-run 预览会显示的差异摘要。
//! 用法：cargo run --bin import_diag -- <当前配置.json> <待导入配置.json>
//!      （省略参数时用 %APPDATA%/com.local.knowledgedock/config.json 作为当前配置）
use knowledge_dock_lib::config::Config;

fn read(path: &str) -> Result<Config, String> {
    let json = std::fs::read_to_string(path).map_err(|e| format!("{path}: {e}"))?;
    serde_json::from_str(&json).map_err(|e| format!("{path}: 解析失败 {e}"))
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let cur_path = args.get(0).cloned().unwrap_or_else(|| {
        let appdata = std::env::var("APPDATA").unwrap_or_default();
        format!("{appdata}/com.local.knowledgedock/config.json")
    });
    let Some(incoming_path) = args.get(1).cloned() else {
        eprintln!("用法: import_diag <当前配置.json> <待导入配置.json>");
        std::process::exit(2);
    };
    let cur = match read(&cur_path) {
        Ok(c) => c,
        Err(e) => {
            println!("CUR_ERR: {e}");
            std::process::exit(1);
        }
    };
    let incoming = match read(&incoming_path) {
        Ok(c) => c,
        Err(e) => {
            println!("INCOMING_ERR: {e}");
            std::process::exit(1);
        }
    };
    let (summary, warnings) = knowledge_dock_lib::diff_configs(&cur, &incoming);
    println!("SUMMARY {} 行:", summary.len());
    for line in &summary {
        println!("  • {line}");
    }
    if !warnings.is_empty() {
        println!("WARNINGS {} 行:", warnings.len());
        for w in &warnings {
            println!("  ! {w}");
        }
    }
}
