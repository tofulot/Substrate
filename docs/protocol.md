# Harness protocol

The transport is UTF-8, newline-delimited JSON-RPC 2.0 over stdin/stdout. Each request is one JSON object followed by `\n`, with a string or number `id`, a `method`, and object `params`. Maximum request frame: 4 MiB. JSON strings escape embedded newlines. One writer task serializes replies and notifications to avoid interleaved output. Diagnostics use stderr only. Requests may complete out of order; clients correlate replies by ID.

```json
{"jsonrpc":"2.0","id":1,"method":"session.create","params":{"project":"/absolute/path/to/repo"}}
{"jsonrpc":"2.0","id":1,"result":{"id":"session-uuid","project":"/absolute/path/to/repo","title":"New conversation","updated_at":0,"messages":[]}}
```

## Requests

| Method | Parameters | Result |
| --- | --- | --- |
| `ping` | none | version and provider |
| `project.open` | `path` | canonical path, name, Git branch |
| `session.create` | `project` | new session |
| `session.list` | none | recent session metadata |
| `session.resume` | `session_id` | persisted messages, tool activities, and running state |
| `agent.start` | `session_id`, `task` | immediate acknowledgment; asynchronous events follow |
| `agent.interrupt`, `session.cancel` | `session_id` | cancellation acknowledgment |
| `tool.approve`, `tool.reject` | `session_id`, `tool_id` | approval acknowledgment |
| `models.list` | none | model objects available to the current key |
| `config.get` | none | model, max_steps, has_key; never the key |
| `config.update` | `model`, optional `max_steps` | save non-secret configuration |
| `config.credentials` | `key` | in-memory credential update; main-process-only, excluded from renderer allowlist |
| `files.list` | `session_id`, `path` (empty string for root) | directory entries |
| `files.read` | `session_id`, `path` | UTF-8 content |
| `git.branch` | `session_id` | current branch, or null |
| `git.status` | `session_id` | changed paths and porcelain statuses |
| `git.file` | `session_id`, `path` | HEAD content and working copy for a diff |

Desktop repository selection uses the main process's native dialog. The Rust service is independently usable by other trusted local clients. Its stdio endpoint is not an authenticated network service.

## Events

Notifications have no `id`. Each agent/tool notification includes `params.session_id`.

- `agent.started`
- `agent.message.delta`: `message_id`, `delta`
- `agent.message.completed`: `message_id`
- `tool.approval.required`: `tool` including validated arguments and optional exact patch preview
- `tool.started`: `tool`
- `tool.output.delta`: `tool_id`, `delta`
- `tool.completed`: `tool` including final status and bounded output
- `agent.completed`
- `agent.failed`: `message`
- `agent.interrupted`: `message`

Tools have `approval`, `running`, `completed`, `failed`, or `rejected` status. Approval IDs are scoped to their session and consumed once. Registering an approval waiter precedes publishing its notification. Closing the app cancels jobs; pending approval cards are displayed as interrupted after restoration.

Parse/envelope failures use `-32700`; method/validation/runtime failures use `-32000`. Provider errors expose a sanitized status and remediation text, rather than echoing upstream response bodies or credentials.
