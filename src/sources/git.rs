//! `git-paths`: a git repository where a file's path says what it belongs to and what state it is in.
//!
//! {"type": "git-paths", "repo": "~/kanban",
//!  "patterns": ["{group}/{state}/{label}/**", …],
//!  "entity": "card-{label}"}
//!
//! `{label}` is required; `{state}` makes a `state` event whenever an entity's state changes;
//! `{group}` colours its card. Every commit touching an entity gives it an `activity` event.

use std::collections::HashMap;
use std::process::Command;

use anyhow::{Context, Result, bail};
use serde_json::{Value, json};

use crate::pattern::{Pattern, expand_home, fill};
use crate::store::Event;

pub struct Config {
    repo: String,
    patterns: Vec<Pattern>,
    entity: String,
}

impl Config {
    pub fn parse(v: &Value) -> Result<Self> {
        let repo = expand_home(v["repo"].as_str().context("`repo` is required")?);
        let raw: Vec<&str> = v["patterns"]
            .as_array()
            .context("`patterns` must be a list of path patterns")?
            .iter()
            .map(|p| p.as_str().context("patterns must be strings"))
            .collect::<Result<_>>()?;
        if raw.is_empty() {
            bail!("`patterns` is empty");
        }
        if let Some(p) = raw.iter().find(|p| !p.contains("{label}")) {
            bail!("pattern {p:?} has no {{label}}");
        }
        Ok(Config {
            repo,
            patterns: raw.into_iter().map(Pattern::new).collect::<Result<_>>()?,
            entity: v["entity"].as_str().unwrap_or("{label}").to_string(),
        })
    }

    pub fn head(&self) -> Result<String> {
        Ok(self.git(&["rev-parse", "HEAD"])?.trim().to_string())
    }

    /// Every event the whole history gives; storing them again changes nothing (refs).
    pub fn events(&self, source: &str) -> Result<Vec<Event>> {
        let log = self.git(&[
            "log",
            "--reverse",
            "-M",
            "--name-status",
            "--format=%x1e%at%x09%H%x09%s",
        ])?;
        Ok(self.parse_log(source, &log))
    }

    fn git(&self, args: &[&str]) -> Result<String> {
        let out = Command::new("git")
            .args(["-c", "core.quotePath=false", "-C", &self.repo])
            .args(args)
            .output()
            .context("run git")?;
        if !out.status.success() {
            bail!(
                "git {} in {}: {}",
                args[0],
                self.repo,
                String::from_utf8_lossy(&out.stderr).trim()
            );
        }
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    }

    fn captures(&self, path: &str) -> Option<HashMap<String, String>> {
        self.patterns.iter().find_map(|p| p.captures(path))
    }

    /// `git log --name-status --format=%x1e%at%x09%H%x09%s` → events, oldest first.
    fn parse_log(&self, source: &str, log: &str) -> Vec<Event> {
        let mut events = Vec::new();
        let mut state: HashMap<String, String> = HashMap::new();
        for commit in log.split('\x1e').filter(|c| !c.trim().is_empty()) {
            let mut lines = commit.lines();
            let mut head = lines.next().unwrap_or_default().splitn(3, '\t');
            let (Some(at), Some(hash), subject) =
                (head.next(), head.next(), head.next().unwrap_or(""))
            else {
                continue;
            };
            let Ok(at) = at.parse::<i64>() else { continue };
            let at = at * 1000;

            // Per entity in this commit: its captures (state from any file that has one) and file count.
            let mut touched: Vec<(String, HashMap<String, String>, usize)> = Vec::new();
            for line in lines.filter(|l| !l.is_empty()) {
                // A rename lists the old and the new path; the last one is where the file is now.
                let Some(caps) = line.split('\t').next_back().and_then(|p| self.captures(p)) else {
                    continue;
                };
                let Some(entity) = fill(&self.entity, &caps) else {
                    continue;
                };
                match touched.iter_mut().find(|t| t.0 == entity) {
                    Some(t) => {
                        t.2 += 1;
                        for (k, v) in caps {
                            t.1.entry(k).or_insert(v);
                        }
                    }
                    None => touched.push((entity, caps, 1)),
                }
            }
            for (entity, caps, files) in touched {
                let mut data = json!({ "label": caps["label"], "commit": &hash[..hash.len().min(7)], "subject": subject, "files": files });
                if let Some(group) = caps.get("group") {
                    data["group"] = json!(group);
                }
                events.push(Event {
                    at,
                    source: source.into(),
                    entity: entity.clone(),
                    kind: "activity".into(),
                    data: data.clone(),
                    r#ref: Some(format!("{hash}/{entity}/activity")),
                });
                let Some(now) = caps.get("state") else {
                    continue;
                };
                let before = state.insert(entity.clone(), now.clone());
                if before.as_ref() != Some(now) {
                    let mut data = json!({ "from": before, "to": now, "label": caps["label"] });
                    if let Some(group) = caps.get("group") {
                        data["group"] = json!(group);
                    }
                    events.push(Event {
                        at,
                        source: source.into(),
                        entity: entity.clone(),
                        kind: "state".into(),
                        data,
                        r#ref: Some(format!("{hash}/{entity}/state")),
                    });
                }
            }
        }
        events
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kanban() -> Config {
        Config::parse(&json!({
            "repo": "/tmp/unused",
            "patterns": ["{group}/{state}/{label}/**", "{group}/{state}/{label}.md"],
            "entity": "card-{label}"
        }))
        .unwrap()
    }

    /// A move between status folders, a second card in the same commit, an unrelated file, the
    /// flat layout, and a list folder renamed in place.
    const LOG: &str = "\x1e1000\taaaaaaaaaa\tstart dune
A\tbooks/todo/dune/notes.md
A\tbooks/todo/dune/quotes.md
\x1e2000\tbbbbbbbbbb\tmove dune, touch fence
R100\tbooks/todo/dune/notes.md\tbooks/doing/dune/notes.md
M\thome/later/fence/notes.md
M\tdocs/readme.md
\x1e3000\tcccccccccc\tflat done file
A\tgarden/done/tomatoes.md
\x1e4000\tdddddddddd\trename list folder
R100\tgarden/done/tomatoes.md\tallotment/done/tomatoes.md
";

    #[test]
    fn events_from_paths() {
        let got: Vec<String> = kanban()
            .parse_log("board", LOG)
            .iter()
            .map(|e| {
                format!(
                    "{} {} {} {}",
                    e.at / 1000,
                    e.entity,
                    e.kind,
                    e.data["to"].as_str().unwrap_or("")
                )
            })
            .collect();
        assert_eq!(
            got,
            [
                "1000 card-dune activity ",
                "1000 card-dune state todo",
                "2000 card-dune activity ",
                "2000 card-dune state doing",
                "2000 card-fence activity ",
                "2000 card-fence state later",
                "3000 card-tomatoes activity ",
                "3000 card-tomatoes state done",
                "4000 card-tomatoes activity ", // renamed list, same state: no state event
            ]
        );
    }

    #[test]
    fn the_latest_event_carries_the_current_group() {
        let events = kanban().parse_log("board", LOG);
        assert_eq!(events.last().unwrap().data["group"], "allotment");
        assert_eq!(events[1].data["label"], "dune");
    }

    #[test]
    fn config_errors() {
        let err = |v: Value| {
            Config::parse(&v)
                .err()
                .map(|e| e.to_string())
                .unwrap_or_default()
        };
        assert!(err(json!({ "patterns": ["{label}"] })).contains("`repo`"));
        assert!(err(json!({ "repo": "x", "patterns": [] })).contains("empty"));
        assert!(err(json!({ "repo": "x", "patterns": ["{state}/x"] })).contains("{label}"));
    }
}
