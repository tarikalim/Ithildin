---
name: ithildin
description: Read and record time-changing data in Ithildin, a local timeline of events (task boards, agent sessions, any history). Use it to answer "what happened when", "what state was X in at T", to load a new data source, or to record what you do.
---

# Ithildin

Ithildin keeps time-changing data as **events** in one SQLite file on this computer and plays them back
on a timeline: a board of states and a stream of moments. Every source (cards in git, Claude
Code transcripts, anything else) ends up in the same shape, so one set of tools reads all of them.

## The model in one paragraph

An event is `{source, entity, kind, at, data, ref?}`. `source` is a dataset (`board`, `claude`),
`entity` the thing that changes (`card-dune`, a session id), `at` the time. `kind: "state"` holds
until the entity's next state and needs `data.to`; every other kind is a **moment** (a prompt, a
commit, a deploy). `data` is free JSON; a few fields have meaning in the window: `label`, `group`,
`parent`, `links`. Details: [references/contract.md](references/contract.md).

## Reading

1. `list_sources` first: names, kinds, counts, time spans, columns.
2. Then the narrowest tool that answers the question:
   - `snapshot`: which column each entity was in at a moment (a board).
   - `get_entity`: one entity's label, state, links, parent and children, recent history.
   - `count_events`: how much happened, by day, week, entity, kind or group. Use it before pulling
     many events.
   - `query_events`: the events themselves, newest first by default, filtered by time, kind,
     entity (and its children with `include_children`), or text. Long strings come back cut to
     `max_chars`; raise it only for the events you need in full.

Times go in as RFC 3339, local `YYYY-MM-DD HH:MM`, or a date; they come back in local RFC 3339.

## Writing

Pick the first that fits:

1. **The data is already on disk and keeps changing** (a git repo whose paths carry state, JSON
   Lines logs): `add_source`, so Ithildin reads it itself and keeps it current. Always call it with
   `dry_run: true` first, check the counts and sample events, adjust, then save without `dry_run`.
   Types: [references/git-paths.md](references/git-paths.md), [references/jsonl.md](references/jsonl.md).
   Worked configs: [references/recipes.md](references/recipes.md).
2. **Anything else** (an API, a database, your own work): `add_events`, up to 5,000 events per call,
   all checked before any is written. Give each event a `ref` (its id in the origin) so sending it
   again changes nothing and sending it with new data updates it.
3. `describe_source` sets the title and the board's column order.

There is no plug-in or adapter code: if neither built-in type can read the data, translate it into
events and send them with `add_events`.

## Rules of thumb

- One `source` per dataset, one `entity` per thing that changes; keep entity ids stable.
- `state` only for things that sit in one place at a time (a task's column); everything else is a
  moment.
- Set `label` to what a person calls the entity; it names its card and lane. Ids alone are hard to read.
- `group` colours cards and lanes (a repo, a project). `parent` nests a lane under another
  entity's (sub-agents under their session, sub-tasks under their task).
- `remove_source` deletes a source's events for good; a configured source can be rebuilt from its
  files, data sent with `add_events` cannot.
- When a tool fails, its message says what to change; fix that and call again.
