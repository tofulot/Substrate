# Architecture

The React and TypeScript interface displays chat, files, Git diffs, and live command output. Electron starts the Rust server and forwards allowlisted requests from its context-isolated, sandboxed renderer. The server communicates over newline-delimited JSON-RPC on stdin and stdout. See the [harness protocol](protocol.md) for requests, events, and tool contracts.

| Path | Responsibility |
| --- | --- |
| `electron/` | Desktop lifecycle, credential storage, IPC, and Rust process supervision |
| `src/` | React interface, Monaco file and diff views, and command output |
| `shared/` | TypeScript protocol types |
| `crates/agent-server` and `crates/agent-protocol` | Standalone JSON-RPC server and message schemas |
| `crates/agent-core` and `crates/agent-runtime` | Agent loop, events, approvals, and cancellation |
| `crates/agent-context` | Versioned repository snapshots, relevance ranking, and context packing |
| `crates/agent-models` | Model provider and streaming implementation |
| `crates/agent-tools` | File, Git, shell, verification, and web tools |
| `crates/agent-storage` | SQLite conversations, snapshots, and traces |

Run the harness on its own with `cargo run -p agent-server`. Send one JSON object per line, for example `{"jsonrpc":"2.0","id":1,"method":"ping","params":{}}`. Diagnostics go to stderr. `SUBSTRATE_DB` overrides the database path and `SUBSTRATE_API_BASE` overrides the Subquadratic base URL for local fixtures.

## Agent loop

The server captures repository context, sends it with recent conversation history to SubQ Savant, streams the response, and executes validated tool calls before the next model turn. The desktop default is `subq-savant-1.4` through the Subquadratic Chat Completions API. A task can use up to 64 model turns by default, configurable from 1 to 128. The app runs one agent at a time, with tool calls executed sequentially.

**Ask before acting** requests approval for file edits. **Approve all code edits** applies validated patches automatically, including a patch already waiting for approval. Shell commands in the desktop app require separate approval and run with the user's filesystem and network permissions. Commands have a 120-second timeout, bounded output, and cancellation. The terminal displays command output. Edits go through the agent, and files and diffs are available in a read-only viewer.

File tools resolve paths inside the selected workspace and reject traversal, access to Git internals, and escaping symlinks. Edits to existing files require a current snapshot or tool read. File hashes reject stale edits, including changes made while approval is pending. Sequential patches use the version produced by the last successful edit.

## Repository context

The harness captures source when creating a conversation and refreshes it between model turns. Each snapshot records a revision, content hashes, Git HEAD when available, coverage, exclusions, and changed paths. File bodies are deduplicated by hash in SQLite. Open **Insights** to inspect snapshots, compare historical files to the working tree, or refresh context while the agent is idle. A snapshot records individual file reads during a bounded scan, so a file can leave the snapshot because of a filter or budget change.

The scan respects Git ignore rules and skips generated and dependency directories, symlinks, common sensitive paths, private key headers, binary files, and files over 128 KiB. It inspects up to 20,000 entries and 2,000 candidate files, with a 32 MiB read limit and up to 500 explicit exclusions. These filters cover common cases. Review snapshot coverage in Insights.

Local ranking uses paths and source identifiers. In automatic mode, small repositories and tasks with strong local matches use local ranking alone. For other tasks, `subq-snap-1.2` reranks up to 128 shortlisted files through the Decisions API using task text and excerpts of up to 2 KiB per file. The harness reuses accepted scores for unchanged candidates and falls back to local ranking when needed. Successful reads of eligible files missed by the snapshot can give those files priority on later turns.

The repository context budget defaults to 512 KiB and can be set from 32 KiB to 4 MiB. The harness prioritizes applicable repository guidance and can include excerpts of larger files in generation context. Initial capture, freshness checks, and manual refresh use local processing. Generation history has its own 512 KiB budget. When history exceeds that budget, the harness keeps the latest task and recent complete tool exchanges, excerpts large results, and retains the full conversation locally.

## Data and provider requests

- The app stores conversations, source snapshots, and usage traces in a local, unencrypted SQLite database under `~/Documents/Substrate/Data`. Model settings live beside the database. **New folder** creates a workspace under `~/Documents/Substrate/Projects`.
- Saved API keys are encrypted with Electron's `safeStorage` and kept in the OS application data directory. Electron sends decrypted keys to the Rust server over its private stdin pipe.
- Subquadratic receives task history, selected source, and tool results for generation. When Snap is used, it receives excerpts from shortlisted files, including some that may be left out of the generation snapshot. Chat Completions requests set `store: false`.
- The app can fetch public HTTP(S) URLs without a search key. To enable search, save a Brave Search API key in **Settings**. Brave receives the search query. Web results are saved with the conversation and can be sent to Subquadratic as context. See [web retrieval tool limits](protocol.md#web-retrieval-tools).

## Usage traces

Insights reports selection and generation separately, with timing, context revision, request and response sizes, tool activity, approval waits, and token counts when the provider supplies them. Missing counts appear as unknown. Estimated input tokens use request bytes divided by four. Traces store metadata, while source bodies and conversation contents are stored in separate database records. See [context and trace semantics](protocol.md#context-and-trace-semantics).
