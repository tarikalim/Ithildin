# Contract v1: the event

Every way in (`add_events`, `ithildin ingest`, a configured source) produces this shape.

```json
{"source": "board", "entity": "card-dune", "kind": "state",
 "at": "2026-10-16T15:21:00+03:00",
 "data": {"to": "doing", "from": "todo", "label": "Dune", "group": "books",
          "links": [{"label": "Notes", "url": "file:///Users/me/kanban/books/doing/dune.md", "app": "Zed"}]},
 "ref": "a1b2c3/card-dune/state"}
```

| Field | Required | Meaning |
|---|---|---|
| `source` | yes | The dataset. Each source is one board/stream in the window. |
| `entity` | yes | The thing that changes. Stable id. |
| `kind` | yes | `"state"`, or any other word for a moment (`prompt`, `commit`, `deploy`). |
| `at` | yes (MCP: default now) | RFC 3339, local `YYYY-MM-DD[ HH:MM[:SS]]`, or unix ms. |
| `data` | no | JSON object. |
| `ref` | no | Unique within the source. Same ref + same data: nothing happens. Same ref + new data: the event is updated. |

## `data` fields the window reads

| Field | Effect |
|---|---|
| `to` | Required on `state`: the column the entity moves to. |
| `from` | The column it left; shown in the history. |
| `label` | The entity's name on its card, lane and panel. The newest label of any event wins. |
| `group` | Colour: entities of one group share a hue. |
| `parent` | Another entity of the same source; this entity's lane sits under its parent's, behind a toggle. |
| `links` | `[{"label", "url", "app"?}]`, `http(s)://` or `file://`. `app` opens a file in that Mac app (e.g. `Zed`). |
| `subject` | Shown as the event's text in tooltips and history. |

Every other field is metadata: listed in the entity panel, clickable when it is a URL or a path.

## Describing a source

`describe_source {source, title?, columns?}`: the title above the board and the column order of its
state values. Values not listed follow in order of first appearance.
