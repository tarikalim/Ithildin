# Recipes

Configs that work as they are; change the paths and names to yours.

## Kanban cards in a git repo (board)

Cards live in `<repo>/<list>/<status>/<card>.md`, or in a folder per card, moved between status
folders with `git mv`.

```json
{"source": "board", "title": "Kanban",
 "columns": ["todo", "doing", "done"],
 "config": {"type": "git-paths", "repo": "~/kanban",
   "patterns": ["{group}/{state}/{label}/**", "{group}/{state}/{label}.md"],
   "entity": "card-{label}",
   "links": [{"label": "Notes", "url": ["file://{repo}/{group}/{state}/{label}/README.md",
                                        "file://{repo}/{group}/{state}/{label}.md"]}]}}
```

## Claude Code sessions and sub-agents (stream)

Transcripts: `~/.claude/projects/<project>/<session>.jsonl`; sub-agents in
`<session>/subagents/agent-<id>.jsonl` with `agent-<id>.meta.json` (description, parentAgentId).
Lines of `type: user` with string content are prompts, but system-injected text uses the same shape
and must be filtered by prefix.

```json
{"source": "claude", "title": "Claude Code · sessions",
 "config": {"type": "jsonl", "files": "~/.claude/projects/*/**.jsonl",
  "carry": {"label": ["aiTitle", "lastPrompt", "message.content"]},
  "file_as": "transcript", "sidecar": ".meta.json",
  "rules": [
   {"where": {"type": "user", "isMeta": {"not": true}, "isSidechain": {"not": true},
              "message.content": {"is": "string", "not": {"prefix": ["<task-notification>", "<command-", "<local-command-", "<bash-"]}}},
    "at": "timestamp", "entity": "{sessionId}", "kind": "prompt", "group": "{cwd}",
    "fields": {"subject": "message.content", "cwd": "cwd", "branch": "gitBranch"}},
   {"where": {"type": "pr-link"}, "at": "timestamp", "entity": "{sessionId}", "kind": "pr",
    "fields": {"subject": "prUrl", "prUrl": "prUrl"}, "ref": "pr/{sessionId}/{prUrl}"},
   {"where": {"type": "user", "isSidechain": true, "meta.parentAgentId": {"exists": false},
              "message.content": {"is": "string", "not": {"prefix": ["<system-reminder>", "[SYSTEM NOTIFICATION", "<task-notification>"]}}},
    "at": "timestamp", "entity": "{sessionId}/{agentId}", "kind": "message",
    "label": "{meta.description}", "group": "{cwd}", "parent": "{sessionId}",
    "fields": {"subject": "message.content", "agentType": "meta.agentType"}},
   {"where": {"type": "user", "isSidechain": true, "meta.parentAgentId": {"exists": true},
              "message.content": {"is": "string", "not": {"prefix": ["<system-reminder>", "[SYSTEM NOTIFICATION", "<task-notification>"]}}},
    "at": "timestamp", "entity": "{sessionId}/{agentId}", "kind": "message",
    "label": "{meta.description}", "group": "{cwd}", "parent": "{sessionId}/{meta.parentAgentId}",
    "fields": {"subject": "message.content", "agentType": "meta.agentType"}}],
  "links": [{"label": "Transcript", "url": "file://{transcript}", "app": "Zed"},
            {"label": "Pull request", "url": "{prUrl}"}]}}
```

## Data from elsewhere (add_events)

An issue tracker's revisions as states, with the revision id as `ref`:

```json
{"events": [
  {"source": "tracker", "entity": "item-42", "kind": "state", "at": "2026-10-15T09:12:00+03:00",
   "data": {"to": "active", "from": "new", "label": "Fix login", "group": "web",
            "links": [{"label": "Item", "url": "https://tracker.example/items/42"}]},
   "ref": "item-42/rev-7"}]}
```

Your own work as moments: a run with its steps nested under it (`parent` names an entity of the
same source):

```json
{"events": [
  {"source": "agent-log", "entity": "run-17", "kind": "start", "at": "2026-10-17 09:00",
   "data": {"label": "Docs rebuild"}},
  {"source": "agent-log", "entity": "run-17/step-1", "kind": "step", "at": "2026-10-17 09:02",
   "data": {"label": "Build", "parent": "run-17", "subject": "ran the docs build"}}]}
```
