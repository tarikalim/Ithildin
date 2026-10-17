//! `jsonl`: JSON Lines files, read incrementally; matching lines become events.
//!
//! {"type": "jsonl", "files": "~/.claude/projects/*/*.jsonl",
//!  "carry": {"label": ["aiTitle", "lastPrompt"]},  // optional: a value seen on any line goes onto
//!                                   // the file's later events; a fallback field only until the first one comes
//!  "file_as": "transcript",         // optional: each event gets the file's path under this data key
//!  "sidecar": ".meta.json",         // optional: x.jsonl's x.meta.json object, as `meta.*` fields
//!  "rules": [{                      // or one rule's keys directly at the top level
//!    "where": {"type": "user", "message.content": {"is": "string"}, "isMeta": {"exists": false}},
//!    "at": "timestamp",             // field holding the time (RFC 3339, local or unix ms)
//!    "entity": "{sessionId}",       // template over the line's fields (dots reach into objects)
//!    "kind": "prompt",              // a moment kind, or "state" together with "state": "{field}"
//!    "label": "…", "group": "…",    // optional templates; "parent": "…" nests the entity's lane
//!    "fields": {"subject": "message.content"},  // optional: data key → field (or a list of fields)
//!    "ref": "{sessionId}/{prUrl}"   // optional: repeated lines are one event, the first one seen
//!  }]}
//!
//! `where` values: a literal (equal), or an object whose keys must all hold:
//!   "is": "string"|"number"|"bool"|"object"|"array", "exists": bool,
//!   "prefix": "x" or ["x", "y"] (a string starting with any of them), "not": a value or condition.
//! Only complete lines are read; the byte offset after the last one is kept per file, so a
//! growing file is read once and a half-written last line waits for the next round.

use std::collections::HashMap;
use std::fs::File;
use std::io::{Read as _, Seek as _, SeekFrom};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde_json::{Map, Value};

use crate::contract;
use crate::ingest::Counts;
use crate::pattern::{Pattern, expand_home, fill_with, slashes};
use crate::store::{Event, Store};

pub struct Config {
    root: PathBuf,
    files: Pattern,
    rules: Vec<Rule>,
    /// data key → fields in priority order: the last value of the best field seen in a file goes
    /// onto its later events.
    carry: Vec<(String, Vec<String>)>,
    file_as: Option<String>,
    sidecar: Option<String>,
}

struct Rule {
    filter: Vec<(String, Cond)>,
    at: String,
    entity: String,
    kind: String,
    state: Option<String>,
    label: Option<String>,
    group: Option<String>,
    parent: Option<String>,
    /// data key → field.
    fields: Vec<(String, String)>,
    r#ref: Option<String>,
}

enum Cond {
    Equals(Value),
    Is(String),
    Exists(bool),
    Prefix(Vec<String>),
    Not(Box<Cond>),
    All(Vec<Cond>),
}

const OPERATORS: &[&str] = &["is", "exists", "prefix", "not"];

impl Cond {
    fn parse(v: &Value) -> Result<Self> {
        // An object made only of operators is a condition; anything else is a value to equal.
        let Some(ops) = v
            .as_object()
            .filter(|m| !m.is_empty() && m.keys().all(|k| OPERATORS.contains(&k.as_str())))
        else {
            return Ok(Cond::Equals(v.clone()));
        };
        let mut all = Vec::new();
        for (op, arg) in ops {
            all.push(match (op.as_str(), arg) {
                ("is", Value::String(t))
                    if ["string", "number", "bool", "object", "array"].contains(&t.as_str()) =>
                {
                    Cond::Is(t.clone())
                }
                ("is", _) => bail!("`is` must be string, number, bool, object or array, not {arg}"),
                ("exists", Value::Bool(b)) => Cond::Exists(*b),
                ("exists", _) => bail!("`exists` must be true or false"),
                ("prefix", Value::String(p)) => Cond::Prefix(vec![p.clone()]),
                ("prefix", Value::Array(a)) => Cond::Prefix(
                    a.iter()
                        .map(|p| p.as_str().map(str::to_string))
                        .collect::<Option<_>>()
                        .context("`prefix` must be strings")?,
                ),
                ("prefix", _) => bail!("`prefix` must be a string or a list of strings"),
                _ => Cond::Not(Box::new(Cond::parse(arg)?)),
            });
        }
        Ok(Cond::All(all))
    }

    fn holds(&self, found: Option<&Value>) -> bool {
        match self {
            Cond::Equals(want) => found == Some(want),
            Cond::Exists(want) => found.is_some() == *want,
            Cond::Is(t) => found.is_some_and(|v| match t.as_str() {
                "string" => v.is_string(),
                "number" => v.is_number(),
                "bool" => v.is_boolean(),
                "object" => v.is_object(),
                _ => v.is_array(),
            }),
            Cond::Prefix(ps) => found
                .and_then(Value::as_str)
                .is_some_and(|s| ps.iter().any(|p| s.starts_with(p.as_str()))),
            Cond::Not(c) => !c.holds(found),
            Cond::All(cs) => cs.iter().all(|c| c.holds(found)),
        }
    }
}

/// `{"key": "path"}` or `["path", …]` (the path is also the key).
fn carry(v: &Value) -> Result<Vec<(String, Vec<String>)>> {
    let Value::Object(m) = v else {
        return Ok(pairs(v, "carry")?
            .into_iter()
            .map(|(k, f)| (k, vec![f]))
            .collect());
    };
    m.iter()
        .map(|(k, f)| {
            let fields = match f {
                Value::String(s) => Some(vec![s.clone()]),
                Value::Array(a) if !a.is_empty() => {
                    a.iter().map(|s| s.as_str().map(str::to_string)).collect()
                }
                _ => None,
            };
            Ok((
                k.clone(),
                fields
                    .with_context(|| format!("`carry.{k}` must be a field or a list of fields"))?,
            ))
        })
        .collect()
}

fn pairs(v: &Value, name: &str) -> Result<Vec<(String, String)>> {
    match v {
        Value::Null => Ok(vec![]),
        Value::Array(a) => a
            .iter()
            .map(|f| f.as_str().map(|s| (s.to_string(), s.to_string())))
            .collect::<Option<_>>()
            .with_context(|| format!("`{name}` entries must be strings")),
        Value::Object(m) => m
            .iter()
            .map(|(k, f)| f.as_str().map(|s| (k.clone(), s.to_string())))
            .collect::<Option<_>>()
            .with_context(|| format!("`{name}` values must be field names")),
        _ => bail!("`{name}` must be a list of fields or an object of key: field"),
    }
}

impl Rule {
    fn parse(v: &Value) -> Result<Self> {
        let text = |k: &str| v[k].as_str().map(str::to_string);
        let kind = text("kind").context("`kind` is required")?;
        let state = text("state");
        if (kind == "state") != state.is_some() {
            bail!("use \"kind\": \"state\" together with \"state\": \"{{field}}\"");
        }
        let filter = match &v["where"] {
            Value::Null => vec![],
            Value::Object(m) => m
                .iter()
                .map(|(k, c)| Ok((k.clone(), Cond::parse(c)?)))
                .collect::<Result<_>>()?,
            _ => bail!("`where` must be an object of field: condition"),
        };
        Ok(Rule {
            filter,
            at: text("at").context("`at` is required: the field holding the time")?,
            entity: text("entity")
                .context("`entity` is required: a template such as \"{sessionId}\"")?,
            kind,
            state,
            label: text("label"),
            group: text("group"),
            parent: text("parent"),
            fields: pairs(&v["fields"], "fields")?,
            r#ref: text("ref"),
        })
    }
}

impl Config {
    pub fn parse(v: &Value) -> Result<Self> {
        let files = slashes(&expand_home(
            v["files"]
                .as_str()
                .context("`files` is required: a path pattern")?,
        ));
        // Walk from the last directory before the first wildcard.
        let fixed = &files[..files.find(['*', '{']).unwrap_or(files.len())];
        let root = PathBuf::from(&fixed[..fixed.rfind('/').map_or(0, |i| i + 1)]);
        let rules = match &v["rules"] {
            Value::Null => vec![Rule::parse(v)?],
            Value::Array(a) if !a.is_empty() => a
                .iter()
                .enumerate()
                .map(|(i, r)| Rule::parse(r).with_context(|| format!("rule {}", i + 1)))
                .collect::<Result<_>>()?,
            _ => bail!("`rules` must be a non-empty list"),
        };
        Ok(Config {
            files: Pattern::new(&files)?,
            root,
            rules,
            carry: carry(&v["carry"])?,
            file_as: v["file_as"].as_str().map(str::to_string),
            sidecar: v["sidecar"].as_str().map(str::to_string),
        })
    }

    fn paths(&self) -> Vec<PathBuf> {
        let mut found = vec![];
        walk(&self.root, &mut |p| {
            if self
                .files
                .captures(&slashes(&p.to_string_lossy()))
                .is_some()
            {
                found.push(p.to_path_buf());
            }
        });
        found.sort();
        found
    }

    /// Changes when any matching file appears, grows or is rewritten.
    pub fn fingerprint(&self) -> Result<String> {
        let mut fp = String::new();
        for p in self.paths() {
            let m = std::fs::metadata(&p)?;
            fp.push_str(&format!(
                "{}:{}:{:?};",
                p.display(),
                m.len(),
                m.modified().ok()
            ));
        }
        Ok(fp)
    }

    pub fn sync(&self, store: &Store, source: &str) -> Result<Counts> {
        let mut counts = Counts::default();
        for path in self.paths() {
            let file = path.to_string_lossy().into_owned();
            let mut f = File::open(&path).with_context(|| format!("open {file}"))?;
            let len = f.metadata()?.len();
            let mut offset = store.cursor(source, &file)?;
            if len < offset {
                offset = 0; // rewritten or truncated: read it again; refs keep events single
            }
            f.seek(SeekFrom::Start(offset))?;
            let mut buf = Vec::new();
            f.read_to_end(&mut buf)?;
            let complete = buf.iter().rposition(|&b| b == b'\n').map_or(0, |i| i + 1);
            // ponytail: carried values live for this read only; after a restart mid-file they
            // come back with the next line that has them (titles repeat through a session).
            let mut carried: HashMap<String, (usize, String)> = HashMap::new();
            // A missing or broken sidecar leaves `meta.*` empty; rules that need it then skip.
            let sidecar: Option<Value> = self.sidecar.as_ref().and_then(|suffix| {
                let side = file
                    .strip_suffix(".jsonl")
                    .map(|stem| format!("{stem}{suffix}"))?;
                serde_json::from_slice(&std::fs::read(side).ok()?).ok()
            });
            let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
            store.tx(|| {
                let mut start = 0;
                for line in buf[..complete].split_inclusive(|&b| b == b'\n') {
                    let line_offset = offset + start as u64;
                    start += line.len();
                    // Lines that are not JSON or match no rule are skipped, not errors.
                    let Ok(mut v) = serde_json::from_slice::<Value>(line) else { continue };
                    if let (Some(meta), Some(obj)) = (&sidecar, v.as_object_mut()) {
                        obj.insert("meta".into(), meta.clone());
                    }
                    for (key, fields) in &self.carry {
                        for (rank, field) in fields.iter().enumerate() {
                            if let Some(value) = get(&v, field).and_then(scalar)
                                // The first field follows its latest value; a fallback keeps its first one.
                                && carried.get(key).is_none_or(|(best, _)| rank < *best || rank == 0)
                            {
                                carried.insert(key.clone(), (rank, value));
                            }
                        }
                    }
                    for (i, rule) in self.rules.iter().enumerate() {
                        let at = format!("{file}:{line_offset}#{i}");
                        if let Some(mut e) = self.event(rule, source, &v, &at)? {
                            // A rule's own ref names something that repeats (a PR link): keep its
                            // first sighting, so its time is when it first appeared.
                            if rule.r#ref.is_some() {
                                let r = e.r#ref.clone().unwrap_or_default();
                                if !seen.insert(r.clone()) || store.has_ref(source, &r)? {
                                    counts.unchanged += 1;
                                    continue;
                                }
                            }
                            let data = e.data.as_object_mut().expect("event data is an object");
                            for (key, (_, value)) in &carried {
                                data.entry(key.clone()).or_insert_with(|| Value::String(value.clone()));
                            }
                            if let Some(key) = &self.file_as {
                                data.insert(key.clone(), Value::String(file.clone()));
                            }
                            super::count(&mut counts, store.add(&e)?);
                        }
                    }
                }
                store.set_cursor(source, &file, offset + complete as u64)
            })?;
        }
        Ok(counts)
    }

    fn event(&self, rule: &Rule, source: &str, v: &Value, line: &str) -> Result<Option<Event>> {
        if !rule.filter.iter().all(|(k, cond)| cond.holds(get(v, k))) {
            return Ok(None);
        }
        let Some(at) = get(v, &rule.at) else {
            return Ok(None);
        };
        let at = contract::time(at).with_context(|| format!("`{}` in {}", rule.at, line))?;
        let lookup = |name: &str| get(v, name).and_then(scalar);
        let Some(entity) = fill_with(&rule.entity, lookup) else {
            return Ok(None);
        };
        let r#ref = match &rule.r#ref {
            Some(t) => match fill_with(t, lookup) {
                Some(r) => r,
                None => return Ok(None),
            },
            None => line.to_string(),
        };

        let mut data = Map::new();
        let mut put = |key: &str, template: &Option<String>| {
            if let Some(value) = template.as_deref().and_then(|t| fill_with(t, lookup)) {
                data.insert(key.into(), Value::String(value));
            }
        };
        put("to", &rule.state);
        put("label", &rule.label);
        put("group", &rule.group);
        put("parent", &rule.parent);
        for (key, field) in &rule.fields {
            if let Some(value) = get(v, field) {
                data.insert(key.clone(), value.clone());
            }
        }
        if rule.state.is_some() && !data.contains_key("to") {
            return Ok(None);
        }
        Ok(Some(Event {
            at,
            source: source.into(),
            entity,
            kind: rule.kind.clone(),
            data: Value::Object(data),
            r#ref: Some(r#ref),
        }))
    }
}

/// A field by dotted path: `message.role`.
fn get<'a>(v: &'a Value, path: &str) -> Option<&'a Value> {
    path.split('.').try_fold(v, |v, key| v.get(key))
}

fn scalar(v: &Value) -> Option<String> {
    match v {
        Value::String(s) => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        Value::Bool(b) => Some(b.to_string()),
        _ => None,
    }
}

fn walk(dir: &Path, found: &mut impl FnMut(&Path)) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let Ok(kind) = entry.file_type() else {
            continue;
        };
        if kind.is_dir() {
            walk(&entry.path(), found);
        } else if kind.is_file() {
            found(&entry.path());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::io::Write as _;

    #[test]
    fn reads_new_complete_lines_only() -> Result<()> {
        let dir = std::env::temp_dir().join(format!("ithildin-jsonl-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("a"))?;
        let path = dir.join("a/s1.jsonl");
        let config = Config::parse(&json!({
            "files": format!("{}/**/*.jsonl", dir.display()),
            "where": { "type": "user" },
            "at": "timestamp", "entity": "{sessionId}", "kind": "message",
            "label": "{message.role}: {sessionId}", "fields": ["cwd"]
        }))?;
        let store = Store::open_in_memory()?;
        let line = |t: &str, ty: &str| {
            format!(
                "{{\"type\":\"{ty}\",\"timestamp\":\"{t}\",\"sessionId\":\"s1\",\"cwd\":\"/x\",\"message\":{{\"role\":\"user\"}}}}\n"
            )
        };

        let mut f = File::create(&path)?;
        write!(
            f,
            "{}{}not json\n{{\"type\":\"user\",\"timesta",
            line("2026-10-16T10:00:00Z", "user"),
            line("2026-10-16T10:01:00Z", "assistant")
        )?;
        let first = config.sync(&store, "claude")?;
        assert_eq!(
            first.added, 1,
            "one user line; the half-written last line waits"
        );

        // The writer finishes the line and adds another.
        write!(
            f,
            "mp\":\"2026-10-16T10:02:00Z\",\"sessionId\":\"s1\"}}\n{}",
            line("2026-10-16T10:03:00Z", "user")
        )?;
        let second = config.sync(&store, "claude")?;
        assert_eq!(
            (second.added, second.unchanged),
            (2, 0),
            "only the new lines are read"
        );

        let events = store.between("claude", None, None, 0, i64::MAX, 10)?;
        assert_eq!(events[0].data["label"], "user: s1");
        assert_eq!(events[0].data["cwd"], "/x");
        assert!(
            events[1].data.get("label").is_none(),
            "no message.role on that line: no label"
        );

        // A rewritten (shorter) file is read again from the start; refs keep it single.
        std::fs::write(&path, line("2026-10-16T10:00:00Z", "user"))?;
        let again = config.sync(&store, "claude")?;
        assert_eq!((again.added, again.unchanged), (0, 1));
        std::fs::remove_dir_all(&dir)?;
        Ok(())
    }

    /// The shape of a Claude Code transcript: prompts mixed with tool results and meta lines,
    /// a title line without a time that repeats, and a PR link that repeats.
    #[test]
    fn rules_conditions_carry_and_refs() -> Result<()> {
        let dir = std::env::temp_dir().join(format!("ithildin-jsonl-rules-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("p"))?;
        let path = dir.join("p/s1.jsonl");
        let lines = [
            r#"{"type":"user","timestamp":"2026-10-16T10:00:00Z","sessionId":"s1","cwd":"/x","message":{"content":"first prompt"}}"#,
            r#"{"type":"last-prompt","sessionId":"s1","lastPrompt":"first prompt"}"#,
            r#"{"type":"last-prompt","sessionId":"s1","lastPrompt":"untitled yet"}"#,
            r#"{"type":"user","timestamp":"2026-10-16T10:00:04Z","sessionId":"s1","cwd":"/x","message":{"content":"untitled yet"}}"#,
            r#"{"type":"user","timestamp":"2026-10-16T10:00:05Z","sessionId":"s1","message":{"content":[{"type":"tool_result"}]}}"#,
            r#"{"type":"user","timestamp":"2026-10-16T10:00:06Z","sessionId":"s1","isMeta":true,"message":{"content":"caveat"}}"#,
            r#"{"type":"user","timestamp":"2026-10-16T10:00:07Z","sessionId":"s1","message":{"content":"<task-notification>done</task-notification>"}}"#,
            r#"{"type":"ai-title","sessionId":"s1","aiTitle":"Fix the board"}"#,
            r#"{"type":"pr-link","timestamp":"2026-10-16T10:05:00Z","sessionId":"s1","prUrl":"https://github.com/o/r/pull/7"}"#,
            r#"{"type":"last-prompt","sessionId":"s1","lastPrompt":"a later prompt"}"#,
            r#"{"type":"user","timestamp":"2026-10-16T10:06:00Z","sessionId":"s1","cwd":"/x","message":{"content":"second prompt"}}"#,
            r#"{"type":"pr-link","timestamp":"2026-10-16T10:07:00Z","sessionId":"s1","prUrl":"https://github.com/o/r/pull/7"}"#,
        ];
        std::fs::write(&path, lines.join("\n") + "\n")?;
        let config = Config::parse(&json!({
            "files": format!("{}/*/*.jsonl", dir.display()),
            "carry": { "label": ["aiTitle", "lastPrompt"] },
            "file_as": "transcript",
            "rules": [
                { "where": { "type": "user", "message.content": { "is": "string", "not": { "prefix": "<task-notification>" } }, "isMeta": { "not": true } },
                  "at": "timestamp", "entity": "{sessionId}", "kind": "prompt", "group": "{cwd}",
                  "fields": { "subject": "message.content" } },
                { "where": { "type": "pr-link" }, "at": "timestamp", "entity": "{sessionId}", "kind": "pr",
                  "fields": ["prUrl"], "ref": "pr/{sessionId}/{prUrl}" }
            ]
        }))?;
        let store = Store::open_in_memory()?;
        let c = config.sync(&store, "claude")?;
        // 3 prompts + 1 PR; the repeated PR line keeps the first sighting.
        assert_eq!((c.added, c.updated, c.unchanged), (4, 0, 1), "{c:?}");

        let events = store.between("claude", None, None, 0, i64::MAX, 10)?;
        let got: Vec<(&str, Option<&str>, Option<&str>)> = events
            .iter()
            .map(|e| {
                (
                    e.kind.as_str(),
                    e.data["subject"].as_str(),
                    e.data["label"].as_str(),
                )
            })
            .collect();
        // lastPrompt labels until a title comes; a later lastPrompt does not replace the title.
        assert_eq!(
            got,
            [
                ("prompt", Some("first prompt"), None),
                ("prompt", Some("untitled yet"), Some("first prompt")),
                ("pr", None, Some("Fix the board")),
                ("prompt", Some("second prompt"), Some("Fix the board"))
            ]
        );
        assert_eq!(
            events[0].data["transcript"],
            path.to_string_lossy().as_ref()
        );
        assert_eq!(events[2].data["prUrl"], "https://github.com/o/r/pull/7");
        // A later read that meets the PR again (a new line in the file) still changes nothing.
        let mut f = std::fs::OpenOptions::new().append(true).open(&path)?;
        let pr_again = r#"{"type":"pr-link","timestamp":"2026-10-16T11:00:00Z","sessionId":"s1","prUrl":"https://github.com/o/r/pull/7"}"#;
        writeln!(f, "{pr_again}")?;
        let again = config.sync(&store, "claude")?;
        assert_eq!((again.added, again.updated, again.unchanged), (0, 0, 1));
        std::fs::remove_dir_all(&dir)?;
        Ok(())
    }

    /// A Claude Code sub-agent: its transcript sits under the session, its description and
    /// parent agent in a sidecar file.
    #[test]
    fn sidecar_fields_and_parents() -> Result<()> {
        let dir =
            std::env::temp_dir().join(format!("ithildin-jsonl-sidecar-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("p/s1/subagents"))?;
        std::fs::write(dir.join("p/s1.jsonl"), "")?;
        let agent = |id: &str, meta: Value| -> Result<()> {
            let line = json!({ "type": "user", "isSidechain": true, "timestamp": "2026-10-16T10:00:00Z", "sessionId": "s1", "agentId": id, "message": { "content": "brief" } });
            std::fs::write(
                dir.join(format!("p/s1/subagents/agent-{id}.jsonl")),
                format!("{line}\n"),
            )?;
            std::fs::write(
                dir.join(format!("p/s1/subagents/agent-{id}.meta.json")),
                meta.to_string(),
            )?;
            Ok(())
        };
        agent("a", json!({ "description": "Research" }))?;
        agent(
            "b",
            json!({ "description": "Dig deeper", "parentAgentId": "a" }),
        )?;
        let rule = |parent: &str, has_parent: bool| {
            json!({
                "where": { "isSidechain": true, "meta.parentAgentId": { "exists": has_parent } },
                "at": "timestamp", "entity": "{sessionId}/{agentId}", "kind": "message",
                "label": "{meta.description}", "parent": parent
            })
        };
        let config = Config::parse(&json!({
            "files": format!("{}/*/**.jsonl", dir.display()),
            "sidecar": ".meta.json",
            "rules": [rule("{sessionId}", false), rule("{sessionId}/{meta.parentAgentId}", true)]
        }))?;
        let store = Store::open_in_memory()?;
        assert_eq!(config.sync(&store, "claude")?.added, 2);
        let mut got: Vec<(String, String, String)> = store
            .between("claude", None, None, 0, i64::MAX, 10)?
            .into_iter()
            .map(|e| {
                (
                    e.entity,
                    e.data["label"].as_str().unwrap_or_default().into(),
                    e.data["parent"].as_str().unwrap_or_default().into(),
                )
            })
            .collect();
        got.sort();
        assert_eq!(
            got,
            [
                ("s1/a".into(), "Research".into(), "s1".into()),
                ("s1/b".into(), "Dig deeper".into(), "s1/a".into())
            ]
        );
        std::fs::remove_dir_all(&dir)?;
        Ok(())
    }

    #[test]
    fn config_errors() {
        let err = |v: Value| {
            Config::parse(&v)
                .err()
                .map(|e| e.to_string())
                .unwrap_or_default()
        };
        assert!(err(json!({ "at": "t", "entity": "e", "kind": "k" })).contains("`files`"));
        assert!(
            err(json!({ "files": "/x/*.jsonl", "at": "t", "entity": "e", "kind": "state" }))
                .contains("\"state\"")
        );
        assert!(err(json!({ "files": "/x/*.jsonl", "entity": "e", "kind": "k" })).contains("`at`"));
        assert!(err(json!({ "files": "/x/*.jsonl", "rules": [] })).contains("non-empty"));
        assert!(err(json!({ "files": "/x/*.jsonl", "at": "t", "entity": "e", "kind": "k", "where": { "a": { "is": "date" } } })).contains("`is`"));
        assert!(err(json!({ "files": "/x/*.jsonl", "at": "t", "entity": "e", "kind": "k", "where": { "a": { "prefix": 3 } } })).contains("`prefix`"));
    }

    #[test]
    fn conditions() {
        let c = |v: Value| Cond::parse(&v).unwrap();
        let prompt =
            c(json!({ "is": "string", "not": { "prefix": ["<task-notification>", "<command-"] } }));
        assert!(prompt.holds(Some(&json!("fix the board"))));
        assert!(prompt.holds(Some(&json!("<pasted_content> my text"))));
        assert!(!prompt.holds(Some(&json!("<task-notification> done"))));
        assert!(!prompt.holds(Some(&json!([{ "type": "tool_result" }]))));
        // isMeta absent or false, but not true.
        let not_meta = c(json!({ "not": true }));
        assert!(
            not_meta.holds(None)
                && not_meta.holds(Some(&json!(false)))
                && !not_meta.holds(Some(&json!(true)))
        );
        // An object with other keys is a value to equal, not a condition.
        assert!(c(json!({ "role": "user" })).holds(Some(&json!({ "role": "user" }))));
    }
}
