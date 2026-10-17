# Changelog

All notable changes to Ithildin. The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and versions follow [Semantic Versioning](https://semver.org/).

## [Unreleased]

### Added

- MIT OR Apache-2.0 licence, contributing guide, code of conduct, security policy, issue and pull
  request templates, a pull request check (fmt, clippy, tests) and Dependabot.
- README: step-by-step install from a release, including the Gatekeeper step.
- Pull request checks: branch name, commit messages and PR title, no AI co-author or attribution
  (`Claude-Session:` trailers, "Generated with …" in the PR body), gitleaks,
  and `cargo-deny` (RustSec vulnerabilities, licences, crate sources).

### Changed

- Code formatted with `cargo fmt` defaults.

## [0.3.0] - 2026-10-17

### Added

- Settings dialog behind a gear icon: theme, sources (show or hide, add or edit with a folder
  picker, a JSON config and a preview) and the database's size.
- Themes from Zed's theme files; Islands iTerm stays the default.
- Nested lanes: an entity with `data.parent` sits under its parent's lane behind a toggle, joined
  by a dashed fork line. Claude Code sub-agents (`<session>/subagents/*.jsonl`) appear this way,
  in purple tones per parent.
- `jsonl` sources: `sidecar` files read as `meta.*`, a `parent` rule template, and `carry` with
  fallback fields.
- Stream: trackpad pinch zoom (1 minute to 30 days) with ready spans and Reset; a two-finger
  sideways swipe moves through time; short tooltip cards.
- Playback speed from 0.25x to 2x; "Last move" opens the entity it names.
- MCP rebuilt for reading as well as writing: `list_sources`, `query_events`, `count_events`,
  `snapshot`, `get_entity`, `add_events` (up to 5,000, all checked first), `describe_source`,
  `add_source` with `dry_run`, `remove_source`. JSON results; long strings cut to `max_chars`.
- The `ithildin` skill, served over the MCP Skills extension.

### Changed

- Header holds what is shown (source, dates, view, settings); playback, speed and zoom sit by the
  timeline. Every control keeps its place in every view.
- Date picker shows one month.
- `add_event` replaced by `add_events`; `events_between` by `query_events`.
- Install updates the app in place, so Spotlight keeps it.

### Fixed

- Component icons (calendar arrows, dropdown carets) did not draw: the asset source was missing.
- Labels with line breaks drew over the lane above.
- A long title pushed the panel's close button out of view.
- Injected system text counted as Claude Code prompts and sub-agent messages.

## [0.2.0] - 2026-10-17

### Added

- Claude Code sessions as a `jsonl` source: prompts and pull requests, with links to the
  transcript (opened in a chosen app) and the PR.
- Group colours from Radix Colors.
- README.

## [0.1.0] - 2026-10-16

### Added

- Event store (SQLite), contract v1, `ithildin ingest` and an MCP server.
- Built-in `git-paths` and `jsonl` source types, read in the background and kept current.
- Timeline window: board of states, stream of moments, date range, entity panel with links and
  history, Islands iTerm colours.
- App icon, macOS bundle script, release workflow for universal builds.

[Unreleased]: https://github.com/tarikalim/Ithildin/compare/v0.3.0...HEAD
[0.3.0]: https://github.com/tarikalim/Ithildin/compare/v0.2.0...v0.3.0
[0.2.0]: https://github.com/tarikalim/Ithildin/compare/v0.1.0...v0.2.0
[0.1.0]: https://github.com/tarikalim/Ithildin/releases/tag/v0.1.0
