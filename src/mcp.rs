//! `ithildin mcp`: MCP server over stdio (newline-delimited JSON-RPC 2.0), no auth.
//! Tools read and write events; results are JSON (also as structuredContent). The `ithildin`
//! skill (skills/ithildin) is served through the Skills extension.

use std::collections::{BTreeMap, HashMap};
use std::io::{BufRead as _, Write as _};

use anyhow::{Context as _, Result, anyhow, bail};
use chrono::{Datelike as _, Local, TimeZone as _};
use serde_json::{Value, json};
use sha2::{Digest as _, Sha256};

use crate::contract::{self, parse_time};
use crate::history::History;
use crate::ingest::Counts;
use crate::store::{Event, Store};

const PROTOCOL: &str = "2025-06-18";
const SUPPORTED: &[&str] = &["2025-11-25", "2025-06-18", "2025-03-26", "2024-11-05"];
const SKILLS: &str = "io.modelcontextprotocol/skills";

/// The skill's files: (path inside the skill, text).
const SKILL_FILES: &[(&str, &str)] = &[
    ("SKILL.md", include_str!("../skills/ithildin/SKILL.md")),
    (
        "references/contract.md",
        include_str!("../skills/ithildin/references/contract.md"),
    ),
    (
        "references/git-paths.md",
        include_str!("../skills/ithildin/references/git-paths.md"),
    ),
    (
        "references/jsonl.md",
        include_str!("../skills/ithildin/references/jsonl.md"),
    ),
    (
        "references/recipes.md",
        include_str!("../skills/ithildin/references/recipes.md"),
    ),
];

const INSTRUCTIONS: &str = "Ithildin is a local timeline of events (source, entity, kind, at, data) from task boards, \
agent sessions and anything else. Start with list_sources. To read, use the narrowest tool: snapshot (a board at a \
moment), get_entity (one thing), count_events (how much, by day/entity/...), query_events (the events). To bring data \
in: add_source with dry_run first when it is already on disk (git-paths, jsonl), else add_events. \
The `ithildin` skill explains the event contract and source types.";

pub fn run() -> Result<()> {
    let store = Store::open_default()?;
    let mut out = std::io::stdout().lock();
    for line in std::io::stdin().lock().lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        if let Some(resp) = handle(&store, &line) {
            writeln!(out, "{resp}")?;
            out.flush()?;
        }
    }
    Ok(())
}

fn rpc_error(id: Value, code: i64, message: &str) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } })
}

/// One JSON-RPC message in, the response out (None for notifications).
pub fn handle(store: &Store, line: &str) -> Option<Value> {
    let msg: Value = match serde_json::from_str(line) {
        Ok(v) => v,
        Err(e) => return Some(rpc_error(Value::Null, -32700, &format!("parse error: {e}"))),
    };
    let id = msg.get("id").cloned();
    let Some(method) = msg.get("method").and_then(Value::as_str) else {
        return match id {
            Some(id) if msg.get("result").is_none() && msg.get("error").is_none() => {
                Some(rpc_error(id, -32600, "invalid request"))
            }
            _ => None,
        };
    };
    let id = id?;
    let params = msg.get("params").cloned().unwrap_or_else(|| json!({}));
    let capabilities =
        json!({ "tools": { "listChanged": false }, "resources": {}, "extensions": { SKILLS: {} } });
    let cache = |mut v: Value| {
        v["resultType"] = json!("complete");
        v["ttlMs"] = json!(300_000);
        v["cacheScope"] = json!("public");
        v
    };
    let result = match method {
        "initialize" => {
            let asked = params["protocolVersion"].as_str();
            let version = asked.filter(|v| SUPPORTED.contains(v)).unwrap_or(PROTOCOL);
            Ok(json!({
                "protocolVersion": version,
                "capabilities": capabilities,
                "serverInfo": { "name": "ithildin", "version": env!("CARGO_PKG_VERSION") },
                "instructions": INSTRUCTIONS
            }))
        }
        "server/discover" => Ok(
            json!({ "capabilities": capabilities, "serverInfo": { "name": "ithildin", "version": env!("CARGO_PKG_VERSION") }, "instructions": INSTRUCTIONS }),
        ),
        "ping" => Ok(json!({})),
        "tools/list" => Ok(json!({ "tools": tools() })),
        "tools/call" => {
            let name = params["name"].as_str().unwrap_or_default();
            let args = params
                .get("arguments")
                .cloned()
                .unwrap_or_else(|| json!({}));
            if tools().iter().any(|t| t["name"] == name) {
                Ok(match call(store, name, &args) {
                    Ok(v) => {
                        json!({ "content": [{ "type": "text", "text": v.to_string() }], "structuredContent": v, "isError": false })
                    }
                    Err(e) => {
                        json!({ "content": [{ "type": "text", "text": format!("{e:#}") }], "isError": true })
                    }
                })
            } else {
                Err((-32602, format!("unknown tool: {name}")))
            }
        }
        "skills/list" => Ok(cache(json!({ "skills": [skill_entry()] }))),
        "skills/get" => match params["uri"].as_str() {
            Some(uri) if uri == skill_uri("SKILL.md") => {
                Ok(cache(json!({ "skill": skill_entry() })))
            }
            _ => Err((-32602, "unknown skill".into())),
        },
        "resources/list" => Ok(
            json!({ "resources": SKILL_FILES.iter().map(|(path, text)| json!({
            "uri": skill_uri(path), "name": format!("ithildin/{path}"), "mimeType": "text/markdown", "size": text.len()
        })).collect::<Vec<_>>() }),
        ),
        "resources/read" => match SKILL_FILES
            .iter()
            .find(|(path, _)| params["uri"].as_str() == Some(&skill_uri(path)))
        {
            Some((path, text)) => Ok(cache(
                json!({ "contents": [{ "uri": skill_uri(path), "mimeType": "text/markdown", "text": text }] }),
            )),
            None => Err((-32602, "unknown resource".into())),
        },
        _ => Err((-32601, format!("method not found: {method}"))),
    };
    Some(match result {
        Ok(r) => json!({ "jsonrpc": "2.0", "id": id, "result": r }),
        Err((code, m)) => rpc_error(id, code, &m),
    })
}

fn skill_uri(path: &str) -> String {
    format!("skill://ithildin/{path}")
}

/// The skills/list entry: frontmatter and a manifest of every file with its digest and size.
fn skill_entry() -> Value {
    let skill = SKILL_FILES[0].1;
    let mut frontmatter = serde_json::Map::new();
    for line in skill.split("---").nth(1).unwrap_or_default().lines() {
        if let Some((k, v)) = line.split_once(':') {
            frontmatter.insert(k.trim().into(), json!(v.trim()));
        }
    }
    json!({
        "uri": skill_uri("SKILL.md"),
        "frontmatter": frontmatter,
        "resources": SKILL_FILES.iter().map(|(path, text)| json!({
            "uri": skill_uri(path),
            "digest": sha256(text),
            "size": text.len()
        })).collect::<Vec<_>>()
    })
}

/// `sha256:<hex>` of a file's bytes, as the Skills manifest wants it.
fn sha256(text: &str) -> String {
    let hex: String = Sha256::digest(text.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    format!("sha256:{hex}")
}

fn tool(name: &str, description: &str, props: Value, required: &[&str], read_only: bool) -> Value {
    json!({
        "name": name,
        "description": description,
        "inputSchema": { "type": "object", "properties": props, "required": required, "additionalProperties": false },
        "annotations": { "readOnlyHint": read_only, "destructiveHint": name == "remove_source", "openWorldHint": false }
    })
}

fn tools() -> Vec<Value> {
    let text = |d: &str| json!({ "type": "string", "description": d });
    let time = |d: &str| json!({ "type": "string", "description": format!("{d}. RFC 3339, local `2026-10-16 14:30`, or a date `2026-10-16`") });
    let int = |d: &str| json!({ "type": "integer", "description": d });
    let source = text("Source name, from list_sources");
    let event = json!({
        "type": "object",
        "properties": {
            "source": text("Dataset; may be left out when given at the top level"),
            "entity": text("The thing that changes, a stable id: 'card-dune', a session id"),
            "kind": text("'state' (holds until the entity's next state; needs data.to) or a moment: 'prompt', 'commit', 'deploy', …"),
            "at": time("When it happened (default: now)"),
            "data": { "type": "object", "description": "Free JSON. Read by the window: to (state's column), from, label (name), group (colour), parent (entity to nest under), links [{label, url, app?}], subject (event text). Other fields are metadata." },
            "ref": text("Its id in the origin, unique within the source: sending it again changes nothing, sending new data updates it")
        },
        "required": ["entity", "kind"]
    });
    vec![
        tool(
            "list_sources",
            "Every source: title, how it is fed (git-paths, jsonl, or events sent from outside), kinds with counts, \
             entities, time span, board columns, hidden or not, last read and its error. Call this first.",
            json!({}),
            &[],
            true,
        ),
        tool(
            "query_events",
            "Events of a source, newest first by default. Filter by time, kind, entity (with its nested children), \
             or text in their data. Strings longer than max_chars come back cut; ask again with a larger max_chars \
             for the few events you need in full. `more: true` means the limit cut the result.",
            json!({
                "source": source,
                "from": time("Start (default: the beginning)"),
                "to": time("End (default: the end)"),
                "kind": text("Only this kind"),
                "entity": text("Only this entity"),
                "include_children": { "type": "boolean", "description": "With entity: also everything nested under it (sub-agents, sub-tasks)" },
                "contains": text("Only events whose data contains this text (case-insensitive)"),
                "order": { "type": "string", "enum": ["newest", "oldest"], "description": "Default newest" },
                "limit": int("Max events, default 50, at most 1000"),
                "max_chars": int("Cut strings longer than this, default 300; 0 for no cut")
            }),
            &["source"],
            true,
        ),
        tool(
            "count_events",
            "How many events, grouped by day, week, entity, kind or group, for questions like 'how active was X per \
             day' without pulling the events. Time keys are ascending; others by count, largest first.",
            json!({
                "source": source,
                "by": { "type": "string", "enum": ["day", "week", "entity", "kind", "group"] },
                "from": time("Start (default: the beginning)"),
                "to": time("End (default: the end)"),
                "kind": text("Only this kind"),
                "limit": int("Max groups, default 100")
            }),
            &["source", "by"],
            true,
        ),
        tool(
            "snapshot",
            "The board at a moment: each column (state value, in board order) with the entities in it.",
            json!({ "source": source, "at": time("The moment (default: now)") }),
            &["source"],
            true,
        ),
        tool(
            "get_entity",
            "One entity: label, state at a moment and since when, links (resolved URLs and files), parent, children, \
             and its latest events.",
            json!({
                "source": source,
                "entity": text("Entity id, as in query_events"),
                "at": time("The moment for its state and history (default: now)"),
                "history": int("How many latest events, default 20"),
                "max_chars": int("Cut strings longer than this, default 300; 0 for no cut")
            }),
            &["source", "entity"],
            true,
        ),
        tool(
            "add_events",
            "Record events, 1 to 5000 per call. All are checked first; if any is invalid nothing is written and the \
             errors name each bad event by index. Use for data Ithildin cannot read itself (an API, your own work).",
            json!({
                "source": text("Default source for events that leave it out"),
                "events": { "type": "array", "items": event, "minItems": 1, "maxItems": 5000 }
            }),
            &["events"],
            false,
        ),
        tool(
            "describe_source",
            "Set a source's title and the order of its board columns (its state values).",
            json!({
                "source": source,
                "title": text("Title shown above the board"),
                "columns": { "type": "array", "items": { "type": "string" }, "description": "State values in board order, e.g. [\"new\", \"active\", \"done\"]" }
            }),
            &["source"],
            false,
        ),
        tool(
            "add_source",
            "Make Ithildin read data on disk itself and keep it current: a git repo whose file paths carry state \
             (git-paths) or JSON Lines files (jsonl). Run with dry_run: true first: it reads into a scratch copy and \
             returns counts and sample events without saving. A saved source with a changed config is rebuilt from \
             scratch. Config reference: the ithildin skill.",
            json!({
                "source": source,
                "title": text("Title shown above the board"),
                "columns": { "type": "array", "items": { "type": "string" }, "description": "State values in board order" },
                "config": { "type": "object", "description": "{\"type\": \"git-paths\" | \"jsonl\", …}; see the ithildin skill" },
                "dry_run": { "type": "boolean", "description": "Read and report without saving (default false)" }
            }),
            &["source", "config"],
            false,
        ),
        tool(
            "remove_source",
            "Delete a source: its events, read positions and settings. A configured source can be added again from \
             its files; events sent with add_events are gone for good.",
            json!({ "source": source }),
            &["source"],
            false,
        ),
    ]
}

fn iso(ms: i64) -> String {
    Local
        .timestamp_millis_opt(ms)
        .single()
        .map(|t| t.to_rfc3339_opts(chrono::SecondsFormat::Secs, false))
        .unwrap_or_else(|| ms.to_string())
}

/// Strings longer than `max` chars are cut, saying how much was left out; 0 keeps everything.
fn clip(v: &mut Value, max: usize) {
    match v {
        Value::String(s) if max > 0 && s.chars().count() > max => {
            let rest = s.chars().count() - max;
            *s = format!(
                "{}… [+{rest} chars]",
                s.chars().take(max).collect::<String>()
            );
        }
        Value::Array(a) => a.iter_mut().for_each(|x| clip(x, max)),
        Value::Object(o) => o.values_mut().for_each(|x| clip(x, max)),
        _ => {}
    }
}

fn event_json(e: &Event, history: &History, max: usize) -> Value {
    let mut data = e.data.clone();
    clip(&mut data, max);
    let label = history.label_of(&e.entity);
    let mut v = json!({ "at": iso(e.at), "entity": e.entity, "kind": e.kind, "data": data });
    if label != e.entity {
        let mut label = json!(label);
        clip(&mut label, max);
        v["label"] = label;
    }
    v
}

/// Reads a source config into a scratch database and summarises it, saving nothing: counts per
/// kind, entities, time span and sample events. add_source's dry run and Settings' preview.
pub fn dry_run(d: &contract::Description, config: &Value) -> Result<Value> {
    let parsed = crate::sources::parse(config).context("config")?;
    let scratch = Store::open_in_memory()?;
    scratch.configure(d, config)?;
    let c = crate::sources::sync(&scratch, &d.source, &parsed)?;
    let history = History::load(&scratch, &d.source)?;
    let all = scratch.between(&d.source, None, None, i64::MIN, i64::MAX, i64::MAX)?;
    let mut kinds: HashMap<&str, i64> = HashMap::new();
    for e in &all {
        *kinds.entry(&e.kind).or_default() += 1;
    }
    let entities: std::collections::HashSet<&str> = all.iter().map(|e| e.entity.as_str()).collect();
    let samples: Vec<Value> = all
        .iter()
        .take(3)
        .chain(all.iter().rev().take(2).rev())
        .map(|e| event_json(e, &history, 200))
        .collect();
    Ok(json!({
        "dry_run": true,
        "events": c.added,
        "skipped_repeats": c.unchanged,
        "entities": entities.len(),
        "kinds": kinds,
        "first": all.first().map(|e| iso(e.at)),
        "last": all.last().map(|e| iso(e.at)),
        "samples": samples,
        "next": "if this looks right, call add_source again without dry_run"
    }))
}

fn call(store: &Store, name: &str, args: &Value) -> Result<Value> {
    let req = |k: &str| -> Result<&str> {
        args[k]
            .as_str()
            .filter(|s| !s.trim().is_empty())
            .ok_or_else(|| anyhow!("`{k}` is required"))
    };
    let opt = |k: &str| args[k].as_str().filter(|s| !s.trim().is_empty());
    let time = |k: &str, default: i64| opt(k).map(parse_time).unwrap_or(Ok(default));
    let num = |k: &str, default: i64, max: i64| args[k].as_i64().unwrap_or(default).clamp(0, max);
    let now = Local::now().timestamp_millis();
    let known = |source: &str| -> Result<()> {
        if store.source_settings()?.iter().any(|s| s.name == source) {
            Ok(())
        } else {
            bail!("no source {source:?}; list_sources shows the names")
        }
    };

    match name {
        "list_sources" => {
            let kinds = store.sources()?;
            let entities = store.entity_counts()?;
            let rows: Vec<Value> = store
                .source_settings()?
                .into_iter()
                .map(|s| {
                    let mine: Vec<_> = kinds.iter().filter(|k| k.source == s.name).collect();
                    let description = store.description(&s.name).ok().flatten();
                    json!({
                        "source": s.name,
                        "title": s.title,
                        "fed_by": s.config.as_ref().and_then(|c| c["type"].as_str()).unwrap_or("events sent from outside"),
                        "events": s.events,
                        "entities": entities.get(&s.name).copied().unwrap_or(0),
                        "kinds": mine.iter().map(|k| (k.kind.clone(), json!(k.count))).collect::<serde_json::Map<_, _>>(),
                        "first": mine.iter().map(|k| k.first).min().map(iso),
                        "last": mine.iter().map(|k| k.last).max().map(iso),
                        "columns": description.map(|d| d.columns).unwrap_or_default(),
                        "hidden": s.hidden,
                        "last_read": s.synced_at.map(iso),
                        "error": s.error,
                    })
                })
                .collect();
            Ok(json!({ "sources": rows }))
        }
        "query_events" => {
            let source = req("source")?;
            known(source)?;
            let (from, to) = (time("from", i64::MIN)?, time("to", i64::MAX)?);
            let history = History::load(store, source)?;
            let entities = match (
                opt("entity"),
                args["include_children"].as_bool().unwrap_or(false),
            ) {
                (Some(e), true) => Some(history.subtree(e)),
                (Some(e), false) => Some([e.to_string()].into()),
                (None, _) => None,
            };
            let needle = opt("contains").map(str::to_lowercase);
            // ponytail: filters in memory over the range; push into SQL if a source outgrows memory.
            let mut events: Vec<Event> = store
                .between(source, opt("kind"), None, from, to, i64::MAX)?
                .into_iter()
                .filter(|e| entities.as_ref().is_none_or(|s| s.contains(&e.entity)))
                .filter(|e| {
                    needle
                        .as_ref()
                        .is_none_or(|n| e.data.to_string().to_lowercase().contains(n))
                })
                .collect();
            if opt("order") != Some("oldest") {
                events.reverse();
            }
            let limit = num("limit", 50, 1000) as usize;
            let max = num("max_chars", 300, i64::MAX) as usize;
            let more = events.len() > limit;
            let rows: Vec<Value> = events
                .iter()
                .take(limit)
                .map(|e| event_json(e, &history, max))
                .collect();
            Ok(json!({ "source": source, "returned": rows.len(), "more": more, "events": rows }))
        }
        "count_events" => {
            let source = req("source")?;
            known(source)?;
            let by = req("by")?;
            let events = store.between(
                source,
                opt("kind"),
                None,
                time("from", i64::MIN)?,
                time("to", i64::MAX)?,
                i64::MAX,
            )?;
            let key = |e: &Event| -> String {
                let day = Local
                    .timestamp_millis_opt(e.at)
                    .single()
                    .map(|t| t.date_naive())
                    .unwrap_or_default();
                match by {
                    "day" => day.to_string(),
                    "week" => (day
                        - chrono::Days::new(day.weekday().num_days_from_monday().into()))
                    .to_string(),
                    "entity" => e.entity.clone(),
                    "kind" => e.kind.clone(),
                    _ => e.data["group"].as_str().unwrap_or("(none)").to_string(),
                }
            };
            if !["day", "week", "entity", "kind", "group"].contains(&by) {
                bail!("`by` must be day, week, entity, kind or group");
            }
            let mut counts: BTreeMap<String, i64> = BTreeMap::new();
            for e in &events {
                *counts.entry(key(e)).or_default() += 1;
            }
            let mut rows: Vec<(String, i64)> = counts.into_iter().collect();
            if !matches!(by, "day" | "week") {
                rows.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
            }
            let history = (by == "entity")
                .then(|| History::load(store, source))
                .transpose()?;
            let limit = num("limit", 100, 10_000) as usize;
            let more = rows.len() > limit;
            let groups: Vec<Value> = rows
                .into_iter()
                .take(limit)
                .map(|(k, n)| {
                    let mut v = json!({ "key": k, "count": n });
                    if let Some(h) = &history
                        && h.label_of(&k) != k
                    {
                        v["label"] = json!(h.label_of(&k));
                    }
                    v
                })
                .collect();
            Ok(
                json!({ "source": source, "by": by, "total": events.len(), "more": more, "groups": groups }),
            )
        }
        "snapshot" => {
            let source = req("source")?;
            known(source)?;
            let at = time("at", now)?;
            let history = History::load(store, source)?;
            if !history.has_states() {
                bail!(
                    "{source} has no state events, so it has no board; use query_events or count_events"
                );
            }
            let columns: Vec<Value> = history
                .board_at(at)
                .into_iter()
                .map(|c| {
                    let entities: Vec<Value> = c.cards.iter().map(|card| json!({ "entity": card.entity, "label": card.label, "group": card.group })).collect();
                    json!({ "column": c.status, "count": entities.len(), "entities": entities })
                })
                .collect();
            Ok(json!({ "source": source, "at": iso(at), "columns": columns }))
        }
        "get_entity" => {
            let source = req("source")?;
            known(source)?;
            let entity = req("entity")?;
            let at = time("at", now)?;
            let max = num("max_chars", 300, i64::MAX) as usize;
            let history = History::load(store, source)?;
            let events = history.events_of(entity, i64::MAX, 1);
            if events.is_empty() {
                bail!(
                    "no entity {entity:?} in {source}; query_events or count_events by entity show the ids"
                );
            }
            let state = history
                .state_of(entity, at)
                .map(|e| json!({ "to": e.data["to"], "since": iso(e.at) }));
            let links: Vec<Value> = history
                .links_of(entity)
                .into_iter()
                .map(|l| json!({ "label": l.label, "url": l.url }))
                .collect();
            let children: Vec<Value> = history
                .children_of(entity)
                .into_iter()
                .map(|c| json!({ "entity": c, "label": history.label_of(c) }))
                .collect();
            let recent: Vec<Value> = history
                .events_of(entity, at, num("history", 20, 1000) as usize)
                .into_iter()
                .map(|e| event_json(e, &history, max))
                .collect();
            Ok(json!({
                "source": source,
                "entity": entity,
                "label": history.label_of(entity),
                "state": state,
                "parent": history.parent_of(entity),
                "children": children,
                "links": links,
                "history": recent
            }))
        }
        "add_events" => {
            let list = args["events"]
                .as_array()
                .filter(|a| !a.is_empty())
                .context("`events` must be a non-empty array")?;
            if list.len() > 5000 {
                bail!("{} events; send at most 5000 per call", list.len());
            }
            let mut events = vec![];
            let mut errors = vec![];
            for (i, raw) in list.iter().enumerate() {
                let mut raw = raw.clone();
                if raw["source"].is_null()
                    && let Some(s) = opt("source")
                {
                    raw["source"] = json!(s);
                }
                match contract::event(&raw, Some(now)) {
                    Ok(e) => events.push(e),
                    Err(e) => errors.push(format!("events[{i}]: {e:#}")),
                }
            }
            if !errors.is_empty() {
                let shown = errors
                    .iter()
                    .take(20)
                    .cloned()
                    .collect::<Vec<_>>()
                    .join("\n");
                bail!(
                    "nothing written; {} invalid event(s):\n{shown}",
                    errors.len()
                );
            }
            let c = store.tx(|| {
                let mut c = Counts::default();
                for e in &events {
                    crate::sources::count(&mut c, store.add(e)?);
                }
                Ok(c)
            })?;
            Ok(json!({ "added": c.added, "updated": c.updated, "unchanged": c.unchanged }))
        }
        "describe_source" => {
            let d = contract::description(args)?;
            store.describe(&d)?;
            Ok(json!({ "source": d.source, "title": d.title, "columns": d.columns }))
        }
        "add_source" => {
            let d = contract::description(args)?;
            let config = &args["config"];
            let parsed = crate::sources::parse(config).context("config")?;
            if args["dry_run"].as_bool().unwrap_or(false) {
                return dry_run(&d, config);
            }
            let rebuilt = store.configure(&d, config)?;
            let c = crate::sources::sync(store, &d.source, &parsed)?;
            Ok(
                json!({ "source": d.source, "added": c.added, "updated": c.updated, "unchanged": c.unchanged, "rebuilt": rebuilt, "kept_current": "read again every 5 s while the window is open" }),
            )
        }
        "remove_source" => {
            let source = req("source")?;
            known(source)?;
            Ok(json!({ "source": source, "events_deleted": store.remove_source(source)? }))
        }
        _ => bail!("unknown tool: {name}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rpc(store: &Store, method: &str, params: Value) -> Value {
        let req = json!({ "jsonrpc": "2.0", "id": 1, "method": method, "params": params });
        handle(store, &req.to_string()).unwrap()
    }

    fn call_tool(store: &Store, name: &str, args: Value) -> (Value, String, bool) {
        let r = rpc(
            store,
            "tools/call",
            json!({ "name": name, "arguments": args }),
        )["result"]
            .clone();
        (
            r["structuredContent"].clone(),
            r["content"][0]["text"].as_str().unwrap().to_string(),
            r["isError"].as_bool().unwrap(),
        )
    }

    #[test]
    fn add_query_count_snapshot_entity() -> Result<()> {
        let s = Store::open_in_memory()?;
        let (out, text, err) = call_tool(
            &s,
            "add_events",
            json!({ "source": "tracker", "events": [
                { "entity": "item-1", "kind": "state", "at": "2026-10-16T10:00:00+03:00", "data": { "to": "active", "label": "Fix login" }, "ref": "r1" },
                { "entity": "item-1/a", "kind": "note", "at": "2026-10-16T11:00:00+03:00", "data": { "parent": "item-1", "subject": "x".repeat(400) } },
                { "entity": "item-2", "kind": "state", "at": "2026-10-17T09:00:00+03:00", "data": { "to": "done" } }
            ]}),
        );
        assert!(!err, "{text}");
        assert_eq!(out["added"], 3);
        let (again, _, _) = call_tool(
            &s,
            "add_events",
            json!({ "events": [{ "source": "tracker", "entity": "item-1", "kind": "state", "at": "2026-10-16T10:00:00+03:00", "data": { "to": "active", "label": "Fix login" }, "ref": "r1" }] }),
        );
        assert_eq!(again["unchanged"], 1);

        let (q, _, _) = call_tool(
            &s,
            "query_events",
            json!({ "source": "tracker", "entity": "item-1", "include_children": true }),
        );
        assert_eq!(q["returned"], 2);
        assert_eq!(q["events"][0]["entity"], "item-1/a", "newest first");
        assert!(
            q["events"][0]["data"]["subject"]
                .as_str()
                .unwrap()
                .ends_with("[+100 chars]")
        );

        let (c, _, _) = call_tool(
            &s,
            "count_events",
            json!({ "source": "tracker", "by": "day" }),
        );
        assert_eq!(
            c["groups"],
            json!([{ "key": "2026-10-16", "count": 2 }, { "key": "2026-10-17", "count": 1 }])
        );

        let (snap, _, _) = call_tool(
            &s,
            "snapshot",
            json!({ "source": "tracker", "at": "2026-10-16 12:00" }),
        );
        assert_eq!(snap["columns"][0]["column"], "active");
        assert_eq!(snap["columns"][0]["entities"][0]["label"], "Fix login");

        let (ent, _, _) = call_tool(
            &s,
            "get_entity",
            json!({ "source": "tracker", "entity": "item-1" }),
        );
        assert_eq!(
            (ent["label"].as_str(), ent["children"][0]["entity"].as_str()),
            (Some("Fix login"), Some("item-1/a"))
        );
        assert_eq!(ent["state"]["to"], "active");
        Ok(())
    }

    #[test]
    fn bad_input_is_a_tool_error_and_writes_nothing() -> Result<()> {
        let s = Store::open_in_memory()?;
        let (_, text, err) = call_tool(
            &s,
            "add_events",
            json!({ "source": "x", "events": [
                { "entity": "y", "kind": "z" }, { "entity": "y", "kind": "state" }
            ]}),
        );
        assert!(
            err && text.contains("events[1]") && text.contains("data.to"),
            "{text}"
        );
        assert!(s.source_settings()?.is_empty(), "nothing written");
        let (_, text, err) = call_tool(&s, "query_events", json!({ "source": "nope" }));
        assert!(err && text.contains("list_sources"), "{text}");
        Ok(())
    }

    #[test]
    fn add_source_dry_run_reads_but_saves_nothing() -> Result<()> {
        let dir = std::env::temp_dir().join(format!("ithildin-dry-{}", std::process::id()));
        std::fs::create_dir_all(&dir)?;
        std::fs::write(
            dir.join("a.jsonl"),
            "{\"t\":\"2026-10-16T10:00:00Z\",\"id\":\"x\"}\n{\"t\":\"2026-10-16T11:00:00Z\",\"id\":\"y\"}\n",
        )?;
        let s = Store::open_in_memory()?;
        let config = json!({ "type": "jsonl", "files": format!("{}/*.jsonl", dir.display()), "at": "t", "entity": "{id}", "kind": "ping" });
        let (out, text, err) = call_tool(
            &s,
            "add_source",
            json!({ "source": "pings", "config": config, "dry_run": true }),
        );
        assert!(!err, "{text}");
        assert_eq!(
            (
                out["events"].as_i64(),
                out["entities"].as_i64(),
                out["kinds"]["ping"].as_i64()
            ),
            (Some(2), Some(2), Some(2))
        );
        assert!(s.source_settings()?.is_empty(), "a dry run saves nothing");
        std::fs::remove_dir_all(&dir)?;
        Ok(())
    }

    #[test]
    fn skill_is_served_with_a_matching_manifest() {
        let s = Store::open_in_memory().unwrap();
        let init = rpc(&s, "initialize", json!({ "protocolVersion": "2025-06-18" }));
        assert!(init["result"]["capabilities"]["extensions"][SKILLS].is_object());
        let list = rpc(&s, "skills/list", json!({}));
        let skill = &list["result"]["skills"][0];
        assert_eq!(skill["frontmatter"]["name"], "ithildin");
        for file in skill["resources"].as_array().unwrap() {
            let read = rpc(&s, "resources/read", json!({ "uri": file["uri"] }));
            let text = read["result"]["contents"][0]["text"].as_str().unwrap();
            assert_eq!(file["size"], text.len());
            assert_eq!(file["digest"], sha256(text));
        }
        assert_eq!(
            rpc(&s, "skills/get", json!({ "uri": "skill://nope/SKILL.md" }))["error"]["code"],
            -32602
        );
        // A known vector, so the manifest's digest is not only checked against itself.
        assert_eq!(
            sha256("hello"),
            "sha256:2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824"
        );
    }
}
