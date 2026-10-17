# Ithildin

A native app for macOS, Linux and Windows that keeps time-changing data as events and plays it back on a timeline. Move the playhead and see what was true at that moment: which column each task was in, which Claude Code session was busy, which PR was opened. Written in Rust with [GPUI](https://www.gpui.rs/). Its data lives in one SQLite file on your computer.

The name is Tolkien's *ithildin*, moon-letters: writing that shows only on the right night, under the right moon.

## What it shows

- **Board**: `state` events place each entity in a column. Cards are coloured by group and glow for a while after work on them.
- **Stream**: every other event is a moment, drawn as a dot on its entity's lane around the playhead. Hover a dot for a short card. Pinch the trackpad to zoom (1 minute to 30 days) or pick a span; swipe sideways to move in time.
- **Nested lanes**: an entity with `data.parent` sits under its parent's lane behind a toggle, joined by a dashed fork line. Claude Code sub-agents appear this way, in purple tones per parent.
- **Timeline**: a histogram of events with day marks; drag it, or press Play at 0.25x to 2x (quiet hours are skipped). "Last move" names the latest state change; click it to open that entity.
- **Date range**: opens on this week (Monday to Sunday); presets for last week, the last two weeks and 30 days, or any range.
- **Entity panel**: click a card or a dot for its links (web pages, or files opened in a chosen app), its metadata and its history.
- **Settings** (gear icon): theme, sources (show or hide, add or edit with a preview), and the database's size.

The default theme is the Zed theme *Islands iTerm*; Settings lists every theme in Zed's theme files.
Group colours are Radix Colors' scales, with the purples kept for nested lanes.

## Getting data in

Ithildin knows no source by name. Data enters in one shape, **contract v1** (`src/contract.rs`):

```json
{"at": "2026-10-16T15:21:00+03:00", "source": "board", "entity": "card-dune", "kind": "state",
 "data": {"to": "doing", "label": "Dune", "group": "books",
          "links": [{"label": "Notes", "url": "file:///…/books/doing/dune.md", "app": "Zed"}]},
 "ref": "<a unique id within the source>"}
```

`kind: "state"` holds until the entity's next state and needs `data.to`; any other kind is a moment. A `ref` makes writing the same event again harmless, and new data under the same ref updates it. A source can describe itself: `{"describe": {"source", "title", "columns"}}`.

Three ways in:

| Way | For |
|---|---|
| A **configured source** that Ithildin reads by itself: `git-paths` (a repository whose file paths carry label, state and group) or `jsonl` (JSON Lines files, read incrementally). Added in Settings → Sources, with `ithildin source add`, or by an agent with `add_source` | data that is already on disk and keeps changing |
| `ithildin ingest` — contract JSON Lines on stdin, all lines checked before one transaction | one-off imports, scripts |
| `ithildin mcp` — an MCP server for agents, below | agents that read the data or record what they do |

### MCP

Tools return JSON (also as `structuredContent`); long strings are cut to `max_chars`.

| Tool | For |
|---|---|
| `list_sources` | What exists: kinds, counts, entities, time spans, columns, read errors |
| `query_events` | Events by time, kind, entity (with nested children) or text, newest first |
| `count_events` | Counts by day, week, entity, kind or group |
| `snapshot` | The board at a moment |
| `get_entity` | One entity: label, state, links, parent, children, latest events |
| `add_events` | Up to 5,000 events per call, all checked before any is written |
| `describe_source` | Title and board column order |
| `add_source` | A git-paths or jsonl source Ithildin reads itself; `dry_run` reports without saving |
| `remove_source` | Delete a source |

The `ithildin` skill ([skills/ithildin](skills/ithildin/SKILL.md)) explains the model, the event
contract, both source types and working configs. It is served over the MCP Skills extension
(`skills/list`, `skills/get`, `resources/read`).

Configured sources are caught up when the window opens and checked every 5 seconds while it is open. A changed config rebuilds the source from scratch.

## Install and run

### From a release

[Releases](../../releases) has a universal macOS app (`.dmg` or `.zip`, Apple Silicon and Intel), a Linux x86_64 `.tar.gz` and a Windows x86_64 `.zip`.

**macOS**

1. Open `Ithildin-<version>-macos-universal.dmg` and drag **Ithildin** to **Applications**.
2. The app is signed ad hoc, not notarized, so macOS blocks its first launch. Either open **System Settings → Privacy & Security** and choose **Open Anyway** next to the Ithildin message, or remove the download's quarantine flag once:

   ```sh
   xattr -dr com.apple.quarantine /Applications/Ithildin.app
   ```

**Linux**

1. Extract the archive: `tar -xzf Ithildin-<version>-linux-x86_64.tar.gz`
2. Install the program, launcher and icon for your user:

   ```sh
   install -Dm755 Ithildin-<version>/ithildin ~/.local/bin/ithildin
   install -Dm644 Ithildin-<version>/ithildin.desktop ~/.local/share/applications/ithildin.desktop
   install -Dm644 Ithildin-<version>/ithildin.png ~/.local/share/icons/hicolor/512x512/apps/ithildin.png
   ```

3. Open **Ithildin** from the application menu. `~/.local/bin` must be on your `PATH`.

**Windows**

1. Extract the zip to a folder you keep, for example `%LOCALAPPDATA%\Programs\Ithildin`.
2. Double-click `ithildin.exe`. The program isn't signed: if Windows shows **Windows protected your PC**, click **More info**, then **Run anyway**.

**Optional, the command line and the MCP server for Claude Code** (on Linux the program is already `~/.local/bin/ithildin`):

```sh
# macOS
ln -sf /Applications/Ithildin.app/Contents/MacOS/ithildin ~/.local/bin/ithildin
claude mcp add --scope user ithildin -- /Applications/Ithildin.app/Contents/MacOS/ithildin mcp
# Linux
claude mcp add --scope user ithildin -- ~/.local/bin/ithildin mcp
# Windows
claude mcp add --scope user ithildin -- %LOCALAPPDATA%\Programs\Ithildin\ithildin.exe mcp
```

The window opens empty. Add a source in **Settings → Sources**, or let an agent add one through MCP.

### From source

Rust stable; on Linux, also the packages `scripts/install-linux-deps.sh` installs.

```sh
cargo run --release
scripts/bundle.sh          # macOS: builds release and installs /Applications/Ithildin.app in place
```

Then the optional step above.

### Command line

The app, the CLI and the MCP server are one binary; `ithildin` with no arguments opens the window.

| Command | What it does |
|---|---|
| `ithildin` | Opens the window |
| `ithildin ingest` | Reads contract JSON Lines from stdin |
| `ithildin source add` | Configures a source Ithildin reads itself (JSON on stdin: `source`, `title`, `columns`, `config`) |
| `ithildin sync` | Reads every configured source once |
| `ithildin sources` | Lists sources, kinds, counts and time spans |
| `ithildin mcp` | Runs the MCP server over stdio |

The database is `Ithildin/ithildin.db` in the data folder: `~/Library/Application Support` on macOS, `~/.local/share` on Linux, `%APPDATA%` on Windows (`$ITHILDIN_DB` overrides it).

## Releases

Pushing a `v*` tag runs `.github/workflows/release.yml`: a universal macOS build (`.dmg` and `.zip`), a Linux `.tar.gz` and a Windows `.zip`, and a release with all of them.

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md). Changes are listed in [CHANGELOG.md](CHANGELOG.md).

## Licence

Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or [MIT license](LICENSE-MIT),
at your option. Unless you state otherwise, any contribution you submit for inclusion in Ithildin
is licensed the same way, without any additional terms or conditions.
