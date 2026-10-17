# Contributing

Thanks for helping. Ithildin is a small project, so the process is short.

## Build and run

You need stable Rust; on Linux, also the packages `scripts/install-linux-deps.sh` installs.

```sh
cargo run                  # the window, with a debug build
cargo test
scripts/bundle.sh          # macOS: release build installed as /Applications/Ithildin.app
```

The window reads `ithildin.db` in the app's data folder (`~/Library/Application Support/Ithildin`
on macOS). Point `ITHILDIN_DB` at
another file to try changes without touching your own data.

## Before a pull request

```sh
cargo fmt
cargo clippy --all-targets -- -D warnings
cargo test
cargo deny check           # cargo install cargo-deny
```

The pull request check runs these, and also checks:

- **Branch name**: [Conventional Branch](https://conventionalbranch.org/): `feat/`, `fix/`,
  `hotfix/` or `chore/` + lower-case words joined by hyphens (`feat/pinch-zoom`), or
  `release/<version>`. Documentation changes go on `chore/`. Rule: `scripts/check-branch-name.sh`.
- **Commit messages and the PR title**: [Conventional Commits](https://www.conventionalcommits.org/),
  `feat: …`, `fix: …`, `docs: …`, at most 100 characters in the subject. Rule: `committed.toml`.
  The body says why, with any measurement that led to the change.
- **No AI attribution**: no AI tool as co-author (`Co-authored-by:`), no `Claude-Session:`
  trailer, no "Generated with …" line in the PR body.
- **No secrets** in the new commits (gitleaks).
- **Dependencies**: no known vulnerability (RustSec), only the licences in `deny.toml`.

Clippy and the tests run on macOS and are skipped while the pull request is a draft; `ci.yml` also
builds and tests on Linux and Windows.

- One change per pull request, with a test when it changes behaviour.
- Code follows `cargo fmt`'s defaults. Keep code comments to a line or two; the reasoning goes in
  the commit message.
- Add a line under `Unreleased` in `CHANGELOG.md` for anything a user would notice.

## Where things are

| Path | What |
|---|---|
| `src/contract.rs` | The event shape every input is checked against |
| `src/store.rs` | SQLite store and migrations |
| `src/sources/` | Built-in source types: `git-paths`, `jsonl` |
| `src/history.rs` | Events in memory: board, lanes, nesting |
| `src/ui.rs`, `src/settings.rs`, `src/theme.rs` | The window |
| `src/mcp.rs` | MCP server |
| `skills/ithildin/` | The skill served over MCP; update it when the contract or a source type changes |

## Releases

The maintainer bumps the version in `Cargo.toml`, moves `Unreleased` in `CHANGELOG.md` under the
new version, and pushes a `v*` tag. The release workflow builds and publishes the app.
