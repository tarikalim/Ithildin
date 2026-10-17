//! Built-in source types: Ithildin reads these itself, so a configured source stays current
//! with no adapter, hook or agent. A source's config is stored in the database (`sources.config`).
//!
//! Every config may have `links`: [{"label", "url": template | [templates]}]. The entity panel fills
//! them with the entity's latest values; of several templates the first web URL or existing file wins.

mod git;
mod jsonl;

use std::collections::HashMap;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use serde_json::Value;

use crate::ingest::Counts;
use crate::store::{Store, Write};

/// How often the background reader checks configured sources for changes.
const WATCH_EVERY: Duration = Duration::from_secs(5);

pub enum Config {
    Git(git::Config),
    Jsonl(jsonl::Config),
}

pub fn parse(v: &Value) -> Result<Config> {
    links(v)?;
    match v["type"].as_str() {
        Some("git-paths") => Ok(Config::Git(git::Config::parse(v)?)),
        Some("jsonl") => Ok(Config::Jsonl(jsonl::Config::parse(v)?)),
        other => bail!("unknown source type {other:?}; known: git-paths, jsonl"),
    }
}

pub struct Link {
    pub label: String,
    /// Candidates in order; the first web URL or existing file is used.
    pub urls: Vec<String>,
    /// Opens a file:// link in this app (`open -a`), for files the Mac has no default app for.
    pub app: Option<String>,
}

/// A link ready to click.
#[derive(Debug, Clone, PartialEq)]
pub struct Opened {
    pub label: String,
    pub url: String,
    pub app: Option<String>,
}

pub fn links(config: &Value) -> Result<Vec<Link>> {
    let Some(list) = config.get("links") else {
        return Ok(vec![]);
    };
    list.as_array()
        .context("`links` must be an array")?
        .iter()
        .map(|l| {
            let urls = match &l["url"] {
                Value::String(u) => vec![u.clone()],
                Value::Array(a) => a
                    .iter()
                    .map(|u| u.as_str().map(str::to_string))
                    .collect::<Option<_>>()
                    .context("`url` candidates must be strings")?,
                _ => bail!("a link needs `url`: a template or a list of templates"),
            };
            Ok(Link {
                label: l["label"].as_str().unwrap_or("Link").to_string(),
                urls,
                app: l["app"].as_str().map(str::to_string),
            })
        })
        .collect()
}

/// Values a source's templates may use besides the entity's own fields: `{repo}` for git.
pub fn template_values(config: &Value) -> HashMap<String, String> {
    let mut values = HashMap::new();
    if let Some(repo) = config["repo"].as_str() {
        values.insert("repo".to_string(), crate::pattern::expand_home(repo));
    }
    values
}

/// Links filled with `values`: (label, url) for the first usable candidate of each.
pub fn resolve(links: &[Link], values: &HashMap<String, String>) -> Vec<Opened> {
    links
        .iter()
        .filter_map(|l| {
            let url = l
                .urls
                .iter()
                .filter_map(|u| crate::pattern::fill(u, values))
                .find(|u| match u.strip_prefix("file://") {
                    Some(path) => std::path::Path::new(&path.replace("%20", " ")).exists(),
                    None => true,
                })?;
            Some(Opened {
                label: l.label.clone(),
                url,
                app: l.app.clone(),
            })
        })
        .collect()
}

/// Reads one configured source into the store and records how it went.
pub fn sync(store: &Store, name: &str, config: &Config) -> Result<Counts> {
    let result = match config {
        Config::Git(c) => c.events(name).and_then(|events| {
            store.tx(|| {
                let mut counts = Counts::default();
                for e in &events {
                    count(&mut counts, store.add(e)?);
                }
                Ok(counts)
            })
        }),
        Config::Jsonl(c) => c.sync(store, name),
    };
    let now = chrono::Local::now().timestamp_millis();
    store.synced(
        name,
        now,
        result.as_ref().err().map(|e| format!("{e:#}")).as_deref(),
    )?;
    result
}

pub fn count(c: &mut Counts, w: Write) {
    match w {
        Write::Added => c.added += 1,
        Write::Updated => c.updated += 1,
        Write::Unchanged => c.unchanged += 1,
    }
}

/// Something that changes whenever the source has new data: git's HEAD, the jsonl files' sizes.
fn fingerprint(config: &Config) -> Result<String> {
    match config {
        Config::Git(c) => c.head(),
        Config::Jsonl(c) => c.fingerprint(),
    }
}

/// Reads every configured source whose fingerprint changed since `seen`; errors are recorded per source.
pub fn sync_changed(store: &Store, seen: &mut HashMap<String, String>) -> Result<()> {
    for (name, raw) in store.configured()? {
        // The config is part of the fingerprint: a changed config (Settings) reads again.
        let checked =
            parse(&raw).and_then(|config| Ok((format!("{}|{raw}", fingerprint(&config)?), config)));
        match checked {
            Ok((fp, config)) if seen.get(&name) != Some(&fp) => {
                if sync(store, &name, &config).is_ok() {
                    seen.insert(name, fp);
                }
            }
            Ok(_) => {}
            Err(e) => store.synced(
                &name,
                chrono::Local::now().timestamp_millis(),
                Some(&format!("{e:#}")),
            )?,
        }
    }
    Ok(())
}

/// Keeps configured sources current while the window is open: catches up at start, then
/// every `WATCH_EVERY`. A source missed while the app was closed is caught up the same way.
pub fn watch() {
    std::thread::spawn(|| {
        let store = match Store::open_default() {
            Ok(s) => s,
            Err(e) => return eprintln!("source watcher: {e:#}"),
        };
        let mut seen = HashMap::new();
        loop {
            if let Err(e) = sync_changed(&store, &mut seen) {
                eprintln!("source watcher: {e:#}");
            }
            std::thread::sleep(WATCH_EVERY);
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn links_take_the_first_usable_candidate() -> Result<()> {
        let dir = std::env::temp_dir().join(format!("ithildin-links-{}", std::process::id()));
        std::fs::create_dir_all(&dir)?;
        std::fs::write(dir.join("b.md"), "")?;
        let config = json!({ "links": [
            { "label": "Web", "url": "https://x/{label}/" },
            { "label": "File", "url": [format!("file://{}/a.md", dir.display()), format!("file://{}/{{label}}.md", dir.display())] },
            { "label": "Missing", "url": "https://x/{nothing}" }
        ]});
        let values: HashMap<String, String> = [("label".to_string(), "b".to_string())].into();
        let got = resolve(&links(&config)?, &values);
        let got: Vec<(String, String)> = got.into_iter().map(|o| (o.label, o.url)).collect();
        assert_eq!(
            got,
            vec![
                ("Web".into(), "https://x/b/".into()),
                ("File".into(), format!("file://{}/b.md", dir.display()))
            ]
        );
        std::fs::remove_dir_all(&dir)?;
        Ok(())
    }

    #[test]
    fn unknown_type_is_refused() {
        let err = parse(&json!({ "type": "ftp" })).err().unwrap().to_string();
        assert!(err.contains("git-paths, jsonl"), "{err}");
    }
}
