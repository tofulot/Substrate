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
| `session.resume` | `session_id` | persisted messages, tool activities, running state and active approval mode when running |
| `agent.start` | `session_id`, `task`, optional `approval_mode` (`ask` or `approve_edits`), `check_mode` (`approved` or `isolated`), `context_selection` (`auto`, `local` or `snap`), `context_packing` (`snippets` or `whole_files`), `subq_review` (boolean), and `required_checks` (command strings) | immediate acknowledgment; asynchronous events follow |
| `agent.set_approval_mode` | `session_id`, `approval_mode` (`ask` or `approve_edits`) | updates the active run if present |
| `agent.interrupt`, `session.cancel` | `session_id` | cancellation acknowledgment |
| `tool.approve`, `tool.reject` | `session_id`, `tool_id` | approval acknowledgment |
| `models.list` | none | model objects available to the current key |
| `config.get` | none | model, max_steps, context_budget_bytes, has_key, has_search_key |
| `config.update` | `model`, optional `max_steps` (1–128 model turns, 64 by default), `context_budget_bytes` (32 KiB–4 MiB) | save non-secret configuration |
| `config.credentials` | `key` | in-memory credential update; main-process-only, excluded from renderer allowlist |
| `config.web_search_credentials` | `key` | in-memory Brave Search credential update, excluded from renderer allowlist |
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

An applied patch or a shell command that changes the workspace inventory requires verification before the run can complete. The agent uses `run_check` with a `command` and a `purpose` describing the behavior under test. The check requires command approval, a successful exit, and an unchanged workspace revision. Obvious no-op commands are rejected. When a trusted local caller supplies `required_checks`, each listed command must pass on the current revision. Edits invalidate prior evidence. A declined supplemental check preserves passing evidence, while an executed failure must be resolved on that revision. The harness discards premature final answers and gives the agent a chance to continue. If the agent stops without passing verification, the run emits `agent.failed`.

For bug fixes, the harness prompts the model to check both the reported failure and existing behavior the change could break. It asks the model to compare the original and updated implementations when practical and review coverage after a check passes.

The default `approved` check mode retains ordinary command approvals. The macOS `isolated` profile advertises source navigation, patches, and isolated `run_check`. Commands still need approval. Each check receives a fresh writable `TMPDIR` and `HOME`, a read-only checkout, and no network access. Test commands should disable caches in the checkout or put them in `TMPDIR`. The trusted harness operator can set `SUBSTRATE_CHECK_READ_ROOTS` to a platform-separated list of absolute dependency directories, such as a Python virtual environment and its base runtime. This profile requires the OS sandbox and dash and rejects startup when either is unavailable.

A `run_check` with `kind: "probe"` records exploratory evidence, including a deliberate failure. An explicit `kind: "verification"` participates in the completion gate. In isolated runs with configured required commands, other checks default to probes. A configured required command always participates in verification.

`read_file` accepts optional inclusive, 1-based `start_line` and `end_line` arguments. A ranged result includes line coordinates, the next available line when applicable, and the SHA-256 of the entire file. `search_repository` accepts a workspace-relative `path`, `mode` (`literal` by default, or `regex`), and up to five `context_lines` on each side of a match.

In the approved profile on macOS and Linux, a run that adds a root `package.json` start script must pass `verify_npm_app` before a final answer can report completion. The tool requires command approval, checks installed direct dependencies against npm, and supervises `npm start`. For Electron it checks the installed major against the supported stable release lines and waits for a renderer page before stopping the test launch. A syntax check or an arbitrary shell command does not satisfy this requirement.

In `approve_edits`, validated `apply_patch` calls emit `tool.started` and `tool.completed` with the patch preview. Switching a running task to `approve_edits` also releases a patch awaiting approval. Shell commands require approval in both modes.

Parse/envelope failures use `-32700`; method/validation/runtime failures use `-32000`. Provider errors expose a sanitized status and remediation text, rather than echoing upstream response bodies or credentials.

### Web retrieval tools

- `web_search({query})` returns `query`, `provider`, `retrieved_at`, and up to eight `results` containing `url`, `title`, and `snippet`. It requires a Brave Search key. Indexed snippets can predate the retrieval timestamp.
- `fetch_url({url})` returns `url` after redirects, an optional `title`, `retrieved_at`, `text`, and `truncated`. It accepts public HTTP(S) URLs on standard ports and reads HTML, text, JSON, or XML. It returns up to 24 KiB of UTF-8 text.

Both tools use a 20-second deadline and read at most 1 MiB per response. Fetches validate each redirect and filter DNS results before connecting. Retrieval timestamps use UTC in RFC 3339 format. The agent receives the tool results as untrusted input and is instructed to cite the returned URLs.

## Context and trace semantics

A generation call references the exact immutable snapshot it received. Source bodies live in deduplicated local blobs and are not part of session history or unsolicited events. Snapshot manifests expose coverage limits and exclusions. `context.get` checks freshness without advancing the captured revision; `context.refresh` captures and persists a new revision only when content or context metadata changes. Refresh is rejected while an agent is active, preventing a UI request from racing the agent's capture. The same blob can be shared across revisions, but historical reads require a snapshot associated with the requested session.

New manifests include `selection`: strategy (`local`, `snap`, `snap_with_fallback`, `fallback`), model, task hash, eligible-inventory digest, candidate count, accepted Snap score count, optional fallback reason, and ranks (`path`, file hash, score 0–3, nullable confidence, source). Old manifests may have no selection. Snapshot history summaries omit rank arrays. Inventory hashes cover eligible files and scan exclusions, so changing an unselected file also invalidates selection and freshness. Task hashes contain no task text. A change in selection metadata can create a revision even when included source is unchanged.

Automatic selection uses local ranking for small or high-confidence cases. Snap uses the Decisions API to score up to 128 locally shortlisted, eligible paths. In a task with an inventory change, it reuses accepted scores for matching paths and content hashes within the new shortlist. Only valid local question IDs and finite scores and confidences within bounds are accepted. Confident scores below 1 of 3 exclude files from the packet unless pinned or promoted. On failure, existing accepted scores and local ranks remain available. Creating, inspecting and manually refreshing snapshots incurs no model requests.

A standalone server with `SUBSTRATE_EVAL_CODEX` set to an absolute CLI executable accepts the optional `eval_codex_model` in `agent.start`. The eval CLI supplies one structured generation turn with a five-minute deadline. The server validates and executes every returned tool request. Standard generation turns request medium reasoning. SubQ may use one `max` turn after three turns without new tool evidence or after checking changes involving at least two files or four patches. The review uses the same tools and counts toward the run limit. It has a 90-second deadline. A failed review returns to standard generation when the task can still continue. Use `subq_review: false` for a standard-effort control. SubQ requests up to 65,536 output tokens per turn, including reasoning. Without a SubQ key, the caller must use local selection.

Agent snapshots use snippet packing by default. A file with an `excerpt` includes the displayed line range and retains the full-file hash. The historical file RPC can still return the full snapshot file. Use `context_packing: "whole_files"` for full-file packing. Generation also sees the current tracked diff and a bounded set of checks for the current workspace revision. These remain available after history compaction.

Generation traces report the reasoning effort sent to the provider and the bytes of history supplied to the model. When the 512 KiB history budget shortens a transcript, `history_stored_bytes` records its full size. Stored history and patch-version checks continue to use the original tool results.

Call traces include `kind` (`generation` or `context_selection`, defaulting to generation for old records). Selection traces use model `subq-snap-1.2`. For those calls, `context_bytes` measures selector state, `history_bytes` is zero, and first-output and finish-reason metrics are unavailable. After packing, a selection trace links to the resulting snapshot. Before packing or after interruption, `snapshot_id` can be empty and revision zero. Provider usage includes selection calls even when generation falls back. Raw decision responses and excerpts are omitted from traces.

`CallTrace.metrics.usage` and each individual token field are nullable. Null means unknown; zero is a reported zero. Byte counts, duration, and estimates are also nullable when not observed (for example after a crash). `estimated_input_tokens` is the request-byte count divided by four, rounded up, and is never substituted for provider usage. Calls are persisted before inference and finalized after stream completion, error, or interruption. A successful stream's finish reason does not terminate reading before a possible usage trailer. Run timing separates recorded tool execution and approval waits; a recovered interrupted run may have partial timing. No token prices are assumed.


`RunTrace.context_reads` counts successful `read_file` calls and those whose path was absent from the generation snapshot for that turn. `missed_paths` contains up to 64 unique paths with their first missed snapshot ID and whether they were in its eligible inventory. Successful reads of ineligible files are counted, but do not affect selection. The 16 most recently missed eligible paths take priority within the budget on subsequent turns of that run, after pinned repository guidance. The snapshot's optional `selection.promoted_paths` records eligible paths considered for promotion. Promotion reuses ranking scores and does not trigger a Snap request by itself.
