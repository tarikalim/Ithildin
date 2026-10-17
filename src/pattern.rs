//! Path patterns and templates shared by the built-in source types.
//! Pattern: `{name}` captures part of one path segment, `*` matches within a segment, `**` across them.
//! Template: `{name}` is replaced by a value, e.g. `https://…/edit/{label}/`.

use std::collections::HashMap;

use anyhow::{Context, Result, bail};
use regex::Regex;

pub struct Pattern {
    re: Regex,
}

impl Pattern {
    pub fn new(pattern: &str) -> Result<Self> {
        let mut re = String::from("^");
        let mut rest = pattern;
        while let Some(c) = rest.chars().next() {
            if let Some(after) = rest.strip_prefix("**") {
                re.push_str(".*");
                rest = after;
            } else if c == '*' {
                re.push_str("[^/]*");
                rest = &rest[1..];
            } else if c == '{' {
                let end = rest
                    .find('}')
                    .with_context(|| format!("unclosed `{{` in {pattern:?}"))?;
                let name = &rest[1..end];
                if name.is_empty() || !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
                    bail!("bad placeholder {{{name}}} in {pattern:?}");
                }
                re.push_str(&format!("(?P<{name}>[^/]+?)"));
                rest = &rest[end + 1..];
            } else {
                re.push_str(&regex::escape(&c.to_string()));
                rest = &rest[c.len_utf8()..];
            }
        }
        re.push('$');
        Ok(Pattern {
            re: Regex::new(&re).with_context(|| format!("pattern {pattern:?}"))?,
        })
    }

    /// The placeholders' values when `path` matches.
    pub fn captures(&self, path: &str) -> Option<HashMap<String, String>> {
        let caps = self.re.captures(path)?;
        Some(
            self.re
                .capture_names()
                .flatten()
                .filter_map(|n| Some((n.to_string(), caps.name(n)?.as_str().to_string())))
                .collect(),
        )
    }
}

/// Replaces `{name}` with its value; None when a placeholder has no value.
pub fn fill(template: &str, values: &HashMap<String, String>) -> Option<String> {
    fill_with(template, |name| values.get(name).cloned())
}

/// `fill` with values looked up on demand, so a large JSON line is not flattened first.
pub fn fill_with(template: &str, lookup: impl Fn(&str) -> Option<String>) -> Option<String> {
    let mut out = String::new();
    let mut rest = template;
    while let Some(start) = rest.find('{') {
        out.push_str(&rest[..start]);
        let end = start + rest[start..].find('}')?;
        out.push_str(&lookup(&rest[start + 1..end])?);
        rest = &rest[end + 1..];
    }
    out.push_str(rest);
    Some(out)
}

/// `~/x` → `$HOME/x`.
pub fn expand_home(path: &str) -> String {
    match (path.strip_prefix("~/"), dirs::home_dir()) {
        (Some(rest), Some(home)) => format!("{}/{rest}", home.display()),
        _ => path.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kanban_paths() -> Result<()> {
        let folder = Pattern::new("{group}/{state}/{label}/**")?;
        let flat = Pattern::new("{group}/{state}/{label}.md")?;
        let c = folder
            .captures("books/doing/dune/photos/cover.jpg")
            .unwrap();
        assert_eq!(
            (
                c["group"].as_str(),
                c["state"].as_str(),
                c["label"].as_str()
            ),
            ("books", "doing", "dune")
        );
        assert_eq!(
            flat.captures("garden/done/tomatoes.md").unwrap()["label"],
            "tomatoes"
        );
        // A file directly in the status folder is not a card folder.
        assert!(folder.captures("garden/done/tomatoes.md").is_none());
        assert!(folder.captures("docs/readme.md").is_none());
        Ok(())
    }

    #[test]
    fn stars_and_errors() -> Result<()> {
        let p = Pattern::new("logs/*.jsonl")?;
        assert!(p.captures("logs/a.jsonl").is_some());
        assert!(p.captures("logs/sub/a.jsonl").is_none());
        assert!(
            Pattern::new("logs/**/x.jsonl")?
                .captures("logs/a/b/x.jsonl")
                .is_some()
        );
        assert!(Pattern::new("{unclosed").is_err());
        assert!(Pattern::new("{a-b}").is_err());
        Ok(())
    }

    #[test]
    fn templates() {
        let v: HashMap<String, String> = [("label".to_string(), "7".to_string())].into();
        assert_eq!(
            fill("https://x/edit/{label}/", &v).as_deref(),
            Some("https://x/edit/7/")
        );
        assert_eq!(fill("{missing}", &v), None);
    }
}
