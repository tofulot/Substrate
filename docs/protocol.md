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
| `session.list` | none | all saved session metadata, most recently updated first |
| `session.resume` | `session_id` | persisted messages, tool activities, and running state |
| `agent.start` | `session_id`, `task`, optional `approval_mode` (`ask` by default, or `approve_edits`) | immediate acknowledgment; asynchronous events follow |
| `agent.interrupt`, `session.cancel` | `session_id` | cancellation acknowledgment |
| `tool.approve`, `tool.reject` | `session_id`, `tool_id` | approval acknowledgment |
| `models.list` | none | model objects available to the current key |
| `config.get` | none | model, max_steps, has_key; never the key |
| `config.update` | `model`, optional `max_steps`, `context_budget_bytes` (32 KiB–4 MiB) | save non-secret configuration |
| `config.credentials` | `key` | in-memory credential update; main-process-only, excluded from renderer allowlist |
| `context.get` | `session_id`, optional `snapshot_id` | manifest, current workspace freshness, changed paths, check time; no source bodies |
| `context.refresh` | `session_id` | capture current source when idle; automatic between agent model turns |
| `context.history` | `session_id` | latest 30 snapshot summaries |
| `context.file` | `session_id`, `snapshot_id`, `path` | immutable historical source; snapshot ownership is checked |
| `usage.get` | `session_id` | latest 200 call traces, 30 run traces, total call count |
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
- `context.updated`: `snapshot_id`, `revision`
- `usage.updated`: `call_id`

Tools have `approval`, `running`, `completed`, `failed`, or `rejected` status. Approval IDs are scoped to their session and consumed once. Registering an approval waiter precedes publishing its notification. Closing the app cancels jobs; pending approval cards are displayed as interrupted after restoration.

An applied patch or a shell command that changes the workspace inventory requires verification before the run can complete. The agent uses `run_check` with a `command` and a `purpose` describing the behavior under test. The check requires command approval and succeeds with exit code zero. The harness discards premature final answers and gives the agent a chance to continue. Edits invalidate earlier checks, and failed or declined checks leave verification incomplete. If the agent stops without passing verification, the run emits `agent.failed`.

For bug fixes, the harness prompts the model to check both the reported failure and existing behavior the change could break. It asks the model to compare the original and updated implementations when practical and review coverage after a check passes.

On macOS and Linux, a run that adds a root `package.json` start script must pass `verify_npm_app` before a final answer can report completion. The tool requires command approval, checks installed direct dependencies against npm, and supervises `npm start`. For Electron it checks the installed major against the supported stable release lines and waits for a renderer page before stopping the test launch. A syntax check or an arbitrary shell command does not satisfy this requirement.

The approval mode applies to one agent run. In `approve_edits`, validated `apply_patch` calls emit `tool.started` and `tool.completed` with the patch preview. Shell commands require approval in both modes.

Parse/envelope failures use `-32700`; method/validation/runtime failures use `-32000`. Provider errors expose a sanitized status and remediation text, rather than echoing upstream response bodies or credentials.

## Context and trace semantics

A generation call references the exact immutable snapshot it received. Source bodies live in deduplicated local blobs and are not part of session history or unsolicited events. Snapshot manifests expose coverage limits and exclusions. `context.get` checks freshness without advancing the captured revision; `context.refresh` captures and persists a new revision only when content or context metadata changes. Refresh is rejected while an agent is active, preventing a UI request from racing the agent's capture. The same blob can be shared across revisions, but historical reads require a snapshot associated with the requested session.

New manifests include `selection`: strategy (`local`, `snap`, `snap_with_fallback`, `fallback`), model, task hash, eligible-inventory digest, candidate count, accepted Snap score count, optional fallback reason, and ranks (`path`, file hash, score 0–3, nullable confidence, source). Old manifests may have no selection. Snapshot history summaries omit rank arrays. Inventory hashes cover eligible files and scan exclusions, so changing an unselected file also invalidates selection and freshness. Task hashes contain no task text. A change in selection metadata can create a revision even when included source is unchanged.

Agent turns send up to 128 locally shortlisted, prefiltered files to Snap's `/v1/systemone` endpoint in one batch. Only locally assigned question IDs and finite scores/confidences within bounds are accepted. Confident scores below 1 of 3 exclude unrelated files except pinned guidance. Confidence below 0.6 retains local relevance; missing/extra IDs or provider failures fall back without failing generation. Rankings are reused for an unchanged task and inventory, including a failed attempt within the current run. A later run retries failed selection. Creating, inspecting and manually refreshing snapshots incurs no model requests.

Call traces include `kind` (`generation` or `context_selection`, defaulting to generation for old records). Selection traces use model `subq-snap-1.1`; `context_bytes` measures selector state, `history_bytes` is zero, and first-output/finish-reason metrics are unavailable for this non-streaming endpoint. After packing, a selection trace links to the resulting snapshot. Before packing or after interruption, `snapshot_id` can be empty and revision zero. Provider usage includes selection calls even when generation falls back. Raw decision responses and excerpts are not stored in traces.

`CallTrace.metrics.usage` and each individual token field are nullable. Null means unknown; zero is a reported zero. Byte counts, duration, and estimates are also nullable when not observed (for example after a crash). `estimated_input_tokens` is the request-byte count divided by four, rounded up, and is never substituted for provider usage. Calls are persisted before inference and finalized after stream completion, error, or interruption. A successful stream's finish reason does not terminate reading before a possible usage trailer. Run timing separates recorded tool execution and approval waits; a recovered interrupted run may have partial timing. No token prices are assumed.


`RunTrace.context_reads` counts successful `read_file` calls and those whose path was absent from the generation snapshot for that turn. `missed_paths` contains up to 64 unique paths with their first missed snapshot ID and whether they were in its eligible inventory. Successful reads of ineligible files are counted, but do not affect selection. The 16 most recently missed eligible paths take priority within the budget on subsequent turns of that run, after pinned repository guidance. The snapshot's optional `selection.promoted_paths` records eligible paths considered for promotion. Promotion reuses ranking scores and does not trigger a Snap request by itself.
