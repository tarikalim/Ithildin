//! Contract v1: the only shape data enters Ithildin in, through `ithildin ingest` or MCP.
//! Ithildin knows no source: agents, scripts and the built-in source types all produce this shape.
//!
//! Event: {"at", "source", "entity", "kind", "data"?, "ref"?}
//!   kind "state": holds until the entity's next state; `data.to` (string) is required.
//!   any other kind: a moment.
//!   data fields the window reads, all optional: "label" (card text), "group" (card colour), "from",
//!   "links": [{"label", "url", "app"?}] (http(s):// or file://; a file:// link opens the file on the Mac,
//!   in `app` when given, e.g. "Zed" for a type the Mac has no default app for).
//!   Every other data field is metadata: listed in the entity panel, clickable when it is a URL or a path.
//! Source description: {"describe": {"source", "title"?, "columns"?: [state values in board order]}}

use anyhow::{Context, Result, bail};
use chrono::{DateTime, Local, TimeZone as _};
use serde_json::Value;

use crate::store::Event;

#[derive(Debug, Clone, PartialEq)]
pub struct Description {
    pub source: String,
    pub title: Option<String>,
    pub columns: Vec<String>,
}

pub enum Line {
    Event(Event),
    Describe(Description),
}

/// One JSON Lines line of `ithildin ingest`.
pub fn line(text: &str) -> Result<Line> {
    let v: Value = serde_json::from_str(text).context("not JSON")?;
    match v.get("describe") {
        Some(d) => Ok(Line::Describe(description(d)?)),
        None => Ok(Line::Event(event(&v, None)?)),
    }
}

/// Reads an event; `default_at` fills a missing `at` (MCP uses now, ingest requires it).
pub fn event(v: &Value, default_at: Option<i64>) -> Result<Event> {
    let at = match (&v["at"], default_at) {
        (Value::Null, Some(now)) => now,
        (Value::Null, None) => bail!("`at` is required"),
        (t, _) => time(t)?,
    };
    let data = match &v["data"] {
        Value::Null => Value::Object(Default::default()),
        d @ Value::Object(_) => d.clone(),
        _ => bail!("`data` must be an object"),
    };
    let kind = text(v, "kind")?;
    if kind == "state" && !data["to"].is_string() {
        bail!("a `state` event needs `data.to` (string)");
    }
    match &data["links"] {
        Value::Null => {}
        Value::Array(links)
            if links
                .iter()
                .all(|l| l["url"].is_string() && (l["app"].is_null() || l["app"].is_string())) => {}
        _ => bail!("`data.links` must be an array of {{\"label\", \"url\"}}"),
    }
    Ok(Event {
        at,
        source: text(v, "source")?,
        entity: text(v, "entity")?,
        kind,
        data,
        r#ref: optional(v, "ref"),
    })
}

pub fn description(v: &Value) -> Result<Description> {
    let columns = match &v["columns"] {
        Value::Null => vec![],
        Value::Array(a) => a
            .iter()
            .map(|c| {
                c.as_str()
                    .map(str::to_string)
                    .context("`columns` must be strings")
            })
            .collect::<Result<_>>()?,
        _ => bail!("`columns` must be an array of strings"),
    };
    Ok(Description {
        source: text(v, "source")?,
        title: optional(v, "title"),
        columns,
    })
}

/// A time: unix ms (integer), RFC 3339, or local `YYYY-MM-DD[ HH:MM[:SS]]`.
pub fn time(v: &Value) -> Result<i64> {
    match v {
        Value::Number(n) => n.as_i64().context("`at` in ms must be an integer"),
        Value::String(s) => parse_time(s),
        _ => bail!("a time must be a string or unix ms"),
    }
}

pub fn parse_time(s: &str) -> Result<i64> {
    let s = s.trim();
    if let Ok(t) = DateTime::parse_from_rfc3339(s) {
        return Ok(t.timestamp_millis());
    }
    let naive = [
        "%Y-%m-%d %H:%M:%S",
        "%Y-%m-%d %H:%M",
        "%Y-%m-%dT%H:%M:%S",
        "%Y-%m-%dT%H:%M",
    ]
    .iter()
    .find_map(|f| chrono::NaiveDateTime::parse_from_str(s, f).ok())
    .or_else(|| {
        chrono::NaiveDate::parse_from_str(s, "%Y-%m-%d")
            .ok()
            .and_then(|d| d.and_hms_opt(0, 0, 0))
    })
    .with_context(|| format!("can not read time {s:?}"))?;
    Local
        .from_local_datetime(&naive)
        .earliest()
        .map(|t| t.timestamp_millis())
        .with_context(|| format!("{s:?} does not exist in the local time zone"))
}

pub fn show(ms: i64) -> String {
    Local
        .timestamp_millis_opt(ms)
        .single()
        .map(|t| t.format("%Y-%m-%d %H:%M:%S").to_string())
        .unwrap_or_else(|| ms.to_string())
}

fn text(v: &Value, k: &str) -> Result<String> {
    v[k].as_str()
        .filter(|s| !s.trim().is_empty())
        .map(str::to_string)
        .with_context(|| format!("`{k}` is required"))
}

fn optional(v: &Value, k: &str) -> Option<String> {
    v[k].as_str()
        .filter(|s| !s.trim().is_empty())
        .map(str::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn err(text: &str) -> String {
        match line(text) {
            Ok(_) => String::new(),
            Err(e) => e.to_string(),
        }
    }

    #[test]
    fn reads_events_and_descriptions() -> Result<()> {
        let Line::Event(e) = line(
            r#"{"at":"2026-10-16T10:00:00Z","source":"s","entity":"e","kind":"state","data":{"to":"done"},"ref":"r1"}"#,
        )?
        else {
            panic!("want an event")
        };
        assert_eq!((e.at, e.r#ref.as_deref()), (1_792_144_800_000, Some("r1")));
        let Line::Describe(d) =
            line(r#"{"describe":{"source":"s","title":"S","columns":["new","done"]}}"#)?
        else {
            panic!("want a description")
        };
        assert_eq!(d.columns, ["new", "done"]);
        Ok(())
    }

    #[test]
    fn refuses_what_breaks_the_contract() {
        assert!(err(r#"{"source":"s","entity":"e","kind":"x"}"#).contains("`at`"));
        assert!(err(r#"{"at":1,"source":"s","entity":"e","kind":"state"}"#).contains("data.to"));
        assert!(err(r#"{"at":1,"source":"s","entity":"","kind":"x"}"#).contains("`entity`"));
        assert!(
            err(r#"{"at":1,"source":"s","entity":"e","kind":"x","data":[1]}"#).contains("object")
        );
        assert!(err(r#"{"describe":{"source":"s","columns":"new"}}"#).contains("array"));
        assert!(
            err(
                r#"{"at":1,"source":"s","entity":"e","kind":"x","data":{"links":[{"label":"a"}]}}"#
            )
            .contains("links")
        );
        assert!(err("not json").contains("JSON"));
    }
}
