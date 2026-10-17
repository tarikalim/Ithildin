# Source type `git-paths`

For a git repository where a file's **path** says which entity it belongs to and what state it is
in, e.g. kanban cards moved between `todo/`, `doing/`, `done/`. Ithildin reads `git log` and turns
history into events; it re-reads whenever HEAD moves.

```json
{"type": "git-paths",
 "repo": "~/kanban",
 "patterns": ["{group}/{state}/{label}/**", "{group}/{state}/{label}.md"],
 "entity": "card-{label}",
 "links": [{"label": "Notes", "url": ["file://{repo}/{group}/{state}/{label}/README.md",
                                      "file://{repo}/{group}/{state}/{label}.md"]}]}
```

| Key | Meaning |
|---|---|
| `repo` | Repository path; `~` is the home folder. |
| `patterns` | Path patterns relative to the repo. `{label}` is required. `{state}` makes a `state` event whenever an entity's state changes. `{group}` colours it. `*` matches inside one folder name, `**` across folders. A path matching no pattern is ignored. |
| `entity` | Template for the entity id from the captured values. |
| `links` | Optional, see below. |

Events: each commit touching an entity gives it one `activity` moment (data: `commit`, `subject`, `files`);
a change of `{state}` gives a `state` event. Refs are commit-based, so re-reading adds nothing twice.

## Links (all source types)

`links: [{"label", "url", "app"?}]`. `url` is a template filled with the entity's newest values
(`{label}`, `{state}`, `{group}`, `{repo}`, any data field), or a list of templates: the first web
URL, or the first `file://` that exists, is used. A template with a missing value is skipped.
