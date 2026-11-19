//! 应用搜索：枚举开始菜单快捷方式（.lnk），按名称子串匹配。
//! 不解析 lnk 目标（打开时由 ShellExecute 解析），零额外依赖。

use serde::Serialize;
use std::collections::HashSet;
use std::path::PathBuf;

#[derive(Serialize, Clone)]
pub struct AppHit {
    /// 显示名（lnk 文件名去扩展名）
    pub name: String,
    /// .lnk 完整路径（launch 时交给系统 ShellExecute）
    pub path: String,
}

/// 开始菜单程序目录（全机器 + 当前用户）
fn start_menu_dirs() -> Vec<PathBuf> {
    let mut v = Vec::new();
    if let Ok(pd) = std::env::var("ProgramData") {
        v.push(PathBuf::from(pd).join(r"Microsoft\Windows\Start Menu\Programs"));
    }
    if let Ok(ad) = std::env::var("APPDATA") {
        v.push(PathBuf::from(ad).join(r"Microsoft\Windows\Start Menu\Programs"));
    }
    v
}

/// 递归收集全部 .lnk（跳过卸载器；按显示名去重，先扫到的优先）
pub fn collect() -> Vec<AppHit> {
    let mut out: Vec<AppHit> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    let mut stack: Vec<PathBuf> = start_menu_dirs();
    while let Some(dir) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&dir) else { continue };
        for entry in rd.flatten() {
            let p = entry.path();
            if p.is_dir() {
                stack.push(p);
                continue;
            }
            let is_lnk = p
                .extension()
                .and_then(|e| e.to_str())
                .map(|e| e.eq_ignore_ascii_case("lnk"))
                .unwrap_or(false);
            if !is_lnk {
                continue;
            }
            let stem = p
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("")
                .trim()
                .to_string();
            if stem.is_empty() {
                continue;
            }
            let lower = stem.to_lowercase();
            if lower.contains("卸载") || lower.contains("uninstall") {
                continue;
            }
            if seen.insert(lower) {
                out.push(AppHit {
                    name: stem,
                    path: p.to_string_lossy().to_string(),
                });
            }
        }
    }
    out.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
    out
}

/// 名称子串匹配（大小写不敏感），返回前 limit 条；cache 为 None 时惰性构建
pub fn search(q: &str, limit: usize, cache: &mut Option<Vec<AppHit>>) -> Vec<AppHit> {
    if cache.is_none() {
        *cache = Some(collect());
    }
    let qn = q.trim().to_lowercase();
    if qn.is_empty() {
        return Vec::new();
    }
    let apps = cache.as_ref().expect("apps cache");
    let mut out = Vec::new();
    for a in apps {
        if a.name.to_lowercase().contains(&qn) {
            out.push(a.clone());
            if out.len() >= limit {
                break;
            }
        }
    }
    out
}
