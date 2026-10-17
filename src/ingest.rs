//! `ithildin ingest`: JSON Lines on stdin, one event or source description per line (see contract.rs).
//! All lines are checked before anything is written, so a bad line leaves the database unchanged.

use std::io::BufRead;

use anyhow::{Context, Result};

use crate::contract::{self, Line};
use crate::store::{Store, Write};

#[derive(Debug, Default, PartialEq)]
pub struct Counts {
    pub added: usize,
    pub updated: usize,
    pub unchanged: usize,
    pub described: usize,
}

pub fn run(store: &Store, input: impl BufRead) -> Result<Counts> {
    let mut lines = Vec::new();
    for (i, text) in input.lines().enumerate() {
        let text = text?;
        if text.trim().is_empty() {
            continue;
        }
        lines.push(contract::line(&text).with_context(|| format!("line {}", i + 1))?);
    }
    store.tx(|| {
        let mut c = Counts::default();
        for line in &lines {
            match line {
                Line::Describe(d) => {
                    store.describe(d)?;
                    c.described += 1;
                }
                Line::Event(e) => match store.add(e)? {
                    Write::Added => c.added += 1,
                    Write::Updated => c.updated += 1,
                    Write::Unchanged => c.unchanged += 1,
                },
            }
        }
        Ok(c)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const INPUT: &str = r#"{"describe":{"source":"s","title":"S","columns":["new","done"]}}
{"at":1,"source":"s","entity":"a","kind":"state","data":{"to":"new"},"ref":"1"}

{"at":2,"source":"s","entity":"a","kind":"state","data":{"to":"done"},"ref":"2"}
"#;

    #[test]
    fn ingest_then_ingest_again() -> Result<()> {
        let s = Store::open_in_memory()?;
        assert_eq!(
            run(&s, INPUT.as_bytes())?,
            Counts {
                added: 2,
                described: 1,
                ..Default::default()
            }
        );
        assert_eq!(
            run(&s, INPUT.as_bytes())?,
            Counts {
                unchanged: 2,
                described: 1,
                ..Default::default()
            }
        );
        Ok(())
    }

    #[test]
    fn a_bad_line_writes_nothing() -> Result<()> {
        let s = Store::open_in_memory()?;
        let bad =
            format!("{INPUT}{{\"at\":3,\"source\":\"s\",\"entity\":\"a\",\"kind\":\"state\"}}\n");
        let err = run(&s, bad.as_bytes()).unwrap_err();
        assert!(format!("{err:#}").contains("line 5"), "{err:#}");
        assert!(s.sources()?.is_empty());
        Ok(())
    }
}
