mod contract;
mod history;
mod ingest;
mod mcp;
mod pattern;
mod settings;
mod sources;
mod store;
mod theme;
mod ui;

use std::io::Read as _;

use anyhow::{Context, Result};

const USAGE: &str = "usage:
  ithildin              open the window
  ithildin ingest       read events and source descriptions as JSON Lines from stdin (contract v1)
  ithildin source add   let Ithildin read a source itself; stdin: {\"source\", \"title\"?, \"columns\"?, \"config\"}
  ithildin sync         read every configured source once
  ithildin mcp          MCP server over stdio
  ithildin sources      list what the database holds";

/// Runs a CLI subcommand and exits.
fn exit_with(r: Result<()>) -> ! {
    if let Err(e) = r {
        eprintln!("error: {e:#}");
        std::process::exit(1);
    }
    std::process::exit(0)
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    match args.get(1).map(String::as_str) {
        None => ui::run(),
        Some("mcp") => exit_with(mcp::run()),
        Some("ingest") => exit_with(ingest()),
        Some("sources") => exit_with(sources()),
        Some("source") if args.get(2).map(String::as_str) == Some("add") => exit_with(add_source()),
        Some("sync") => exit_with(sync()),
        _ => {
            eprintln!("{USAGE}");
            std::process::exit(2)
        }
    }
}

fn ingest() -> Result<()> {
    let store = store::Store::open_default()?;
    let c = ingest::run(&store, std::io::stdin().lock())?;
    println!(
        "{} added, {} updated, {} unchanged, {} source description(s)",
        c.added, c.updated, c.unchanged, c.described
    );
    Ok(())
}

fn add_source() -> Result<()> {
    let mut input = String::new();
    std::io::stdin().read_to_string(&mut input)?;
    let args: serde_json::Value = serde_json::from_str(&input).context("stdin is not JSON")?;
    let d = contract::description(&args)?;
    let parsed = sources::parse(&args["config"])?;
    let store = store::Store::open_default()?;
    let rebuilt = store.configure(&d, &args["config"])?;
    let c = sources::sync(&store, &d.source, &parsed)?;
    let note = if rebuilt {
        " (config changed: read again from the start)"
    } else {
        ""
    };
    println!(
        "{}: {} added, {} updated, {} unchanged{note}",
        d.source, c.added, c.updated, c.unchanged
    );
    Ok(())
}

fn sync() -> Result<()> {
    let store = store::Store::open_default()?;
    for (name, config) in store.configured()? {
        match sources::parse(&config).and_then(|c| sources::sync(&store, &name, &c)) {
            Ok(c) => println!(
                "{name}: {} added, {} updated, {} unchanged",
                c.added, c.updated, c.unchanged
            ),
            Err(e) => println!("{name}: FAILED: {e:#}"),
        }
    }
    Ok(())
}

fn sources() -> Result<()> {
    let store = store::Store::open_default()?;
    for s in store.sources()? {
        println!(
            "{:<12} {:<10} {:>6} events  {} → {}",
            s.source,
            s.kind,
            s.count,
            contract::show(s.first),
            contract::show(s.last)
        );
    }
    Ok(())
}
