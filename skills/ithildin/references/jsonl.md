# Source type `jsonl`

For JSON Lines files that grow (logs, transcripts). Ithildin reads each file incrementally: it keeps
the byte offset after the last complete line, so a growing file is read once and a half-written last
line waits. A file that got shorter is read again from the start (refs keep events single).

```json
{"type": "jsonl",
 "files": "~/logs/*/**.jsonl",
 "carry": {"label": ["title", "firstMessage"]},
 "file_as": "transcript",
 "sidecar": ".meta.json",
 "rules": [
   {"where": {"type": "user", "message.content": {"is": "string", "not": {"prefix": ["<system"]}}},
    "at": "timestamp", "entity": "{sessionId}", "kind": "prompt", "group": "{cwd}",
    "fields": {"subject": "message.content", "cwd": "cwd"}},
   {"where": {"type": "link"}, "at": "timestamp", "entity": "{sessionId}", "kind": "pr",
    "fields": {"prUrl": "url"}, "ref": "pr/{sessionId}/{url}"}
 ],
 "links": [{"label": "Transcript", "url": "file://{transcript}", "app": "Zed"}]}
```

## Keys

| Key | Meaning |
|---|---|
| `files` | Path pattern. `*` inside one folder name, `**` across folders (`*/**.jsonl` reaches sub-folders). |
| `rules` | List of rules; each line is checked against every rule. One rule may be written at the top level instead. |
| `carry` | `{data key: field or [fields]}`: a value seen on any line goes onto the file's later events that lack the key. With a list, the first field follows its latest value and later ones are fallbacks that keep their first value until the first field appears. |
| `file_as` | Each event gets the file's path under this data key (for `file://` links). |
| `sidecar` | Suffix of a JSON file next to each `x.jsonl` (`x.meta.json`); its object is available to rules as `meta.*`. |

## A rule

| Key | Meaning |
|---|---|
| `where` | `{field: condition}`, all must hold. Fields use dots: `message.content`, `meta.parentAgentId`. |
| `at` | Field holding the time (RFC 3339, local, or unix ms). Required. |
| `entity` | Template, e.g. `{sessionId}` or `{sessionId}/{agentId}`. Required. A missing value skips the line. |
| `kind` | Moment kind, or `"state"` together with `"state": "{field}"`. |
| `label`, `group`, `parent` | Optional templates into those data fields. |
| `fields` | `{data key: field}` or `[field, …]`: copied into data as they are (any JSON). |
| `ref` | Template naming something that repeats across lines (a PR link): only its first sighting becomes an event. Without it, each line is its own event. |

## Conditions

A literal means "equals". An object holds when all its keys hold:

| Condition | Holds when the field… |
|---|---|
| `{"is": "string"}` | has that JSON type: `string`, `number`, `bool`, `object`, `array`. |
| `{"exists": true}` / `false` | is present / absent. |
| `{"prefix": "x"}` or `["x", "y"]` | is a string starting with any of them. |
| `{"not": c}` | `c` does not hold (`c` a literal or a condition). `{"not": true}` also holds when the field is absent. |

## Checking a config

`add_source` with `dry_run: true` reads everything into a scratch copy and returns counts per kind,
the number of entities, the time span and sample events, without saving. Typical fixes: a `where`
that matches injected or meta lines (add a `not` `prefix`), a template naming a field that is not on
the line (the line is skipped), labels missing (add `carry` or a rule `label`).
