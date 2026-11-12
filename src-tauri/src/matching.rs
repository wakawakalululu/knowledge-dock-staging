//! 范围规则匹配（纯字符串逻辑，零依赖，可被独立工具挂载验证）
//!
//! 规则语义（与 v0.5.x 一致）：
//! - `dir/`        → 前缀：该目录下所有文件
//! - `*.tmp.md`    → 极简通配（按 `*` 分段锚定）
//! - `a/b.md`      → 精确相对路径，或任意深度下的同名末段

/// 单条规则是否命中相对路径（统一正斜杠比较）
pub fn glob_rule_matches(rel: &str, rule: &str) -> bool {
    let rel = rel.replace('\\', "/");
    let r = rule.trim().replace('\\', "/");
    if r.is_empty() {
        return false;
    }
    if r.ends_with('/') {
        return rel.starts_with(&r);
    }
    if r.contains('*') {
        let parts: Vec<&str> = r.split('*').collect();
        let mut rest = rel.as_str();
        let last = parts.len() - 1;
        for (i, part) in parts.iter().enumerate() {
            if part.is_empty() {
                continue;
            }
            if i == 0 {
                if !rest.starts_with(part) {
                    return false;
                }
                rest = &rest[part.len()..];
            } else if i == last {
                if !rest.ends_with(part) {
                    return false;
                }
            } else {
                match rest.find(part) {
                    Some(idx) => rest = &rest[idx + part.len()..],
                    None => return false,
                }
            }
        }
        return true;
    }
    // 精确：全路径相等，或任意深度下的同名末段
    rel == r || rel.ends_with(&format!("/{r}"))
}

/// 排除判定：任一规则命中即排除
pub fn is_excluded_rel(rel: &str, rules: &[String]) -> bool {
    rules.iter().any(|r| glob_rule_matches(rel, r))
}

/// 包含判定（白名单）：规则为空 = 全收；非空 = 至少命中一条
pub fn is_included_rel(rel: &str, rules: &[String]) -> bool {
    rules.is_empty() || rules.iter().any(|r| glob_rule_matches(rel, r))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dir_prefix_rule() {
        assert!(glob_rule_matches("草稿/a.md", "草稿/"));
        assert!(glob_rule_matches("docs/x/y.md", "docs/"));
        assert!(!glob_rule_matches("other/a.md", "docs/"));
    }

    #[test]
    fn wildcard_rule() {
        assert!(glob_rule_matches("a/b.tmp.md", "*.tmp.md"));
        assert!(!glob_rule_matches("a/b.md", "*.tmp.md"));
        assert!(glob_rule_matches("docs/x/y.md", "docs/*.md"));
        assert!(glob_rule_matches("anything.md", "*"));
    }

    #[test]
    fn exact_rule_matches_basename_at_any_depth() {
        assert!(glob_rule_matches("secret.md", "secret.md"));
        assert!(glob_rule_matches("a/b/secret.md", "secret.md"));
        assert!(!glob_rule_matches("secret.md.bak", "secret.md"));
    }

    #[test]
    fn include_semantics() {
        let rules = vec!["docs/".to_string()];
        assert!(is_included_rel("docs/a.md", &rules));
        assert!(!is_included_rel("src/a.md", &rules));
        assert!(is_included_rel("src/a.md", &[]), "空白名单 = 全收");
    }

    #[test]
    fn exclude_semantics() {
        let rules = vec!["drafts/".to_string(), "*.tmp.md".to_string()];
        assert!(is_excluded_rel("drafts/a.md", &rules));
        assert!(is_excluded_rel("x.tmp.md", &rules));
        assert!(!is_excluded_rel("keep/a.md", &rules));
    }
}
