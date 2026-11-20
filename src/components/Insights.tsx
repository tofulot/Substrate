import { useEffect, useState } from "react";
import { Activity, ChevronDown, FileCode2, RefreshCw } from "lucide-react";
import { api, useStore } from "../lib/store";
import type {
  ContextSnapshot,
  ContextState,
  UsageReport,
  TokenUsage,
} from "../../shared/types";
import type { Preview } from "../view-types";

const bytes = (n: number | null) =>
  n == null
    ? "Unknown"
    : n < 1024
      ? `${n} B`
      : `${(n / 1024).toFixed(n < 10240 ? 1 : 0)} KiB`;
const number = (n: number | null | undefined) =>
  n == null ? "Unknown" : n.toLocaleString();
const duration = (n: number | null | undefined) =>
  n == null ? "—" : n < 1000 ? `${n} ms` : `${(n / 1000).toFixed(1)} s`;
type Summary = Omit<ContextSnapshot, "files" | "excluded" | "changes">;
export default function Insights({
  onPreview,
}: {
  onPreview: (preview: Preview) => void;
}) {
  const session = useStore((s) => s.session?.id);
  const revision = useStore((s) => s.revision);
  const insightsRevision = useStore((s) => s.insightsRevision);
  const running = useStore((s) => s.running);
  const [state, setState] = useState<ContextState | null>(null);
  const [history, setHistory] = useState<Summary[]>([]);
  const [usage, setUsage] = useState<UsageReport | null>(null);
  const [selected, setSelected] = useState("");
  const [expanded, setExpanded] = useState<string | null>(null);
  const [filter, setFilter] = useState("");
  const [error, setError] = useState("");
  const [busy, setBusy] = useState(false);
  const [refreshVersion, setRefreshVersion] = useState(0);
  useEffect(() => {
    setSelected("");
    setFilter("");
    setState(null);
    setUsage(null);
  }, [session]);
  useEffect(() => {
    if (!session) return;
    let alive = true;
    let fetching = false;
    async function load() {
      if (fetching) return;
      fetching = true;
      try {
        const [context, versions, report] = await Promise.all([
          api.rpc<ContextState>("context.get", {
            session_id: session,
            ...(selected ? { snapshot_id: selected } : {}),
          }),
          api.rpc<Summary[]>("context.history", { session_id: session }),
          api.rpc<UsageReport>("usage.get", { session_id: session }),
        ]);
        if (alive) {
          setState(context);
          setHistory(versions);
          setUsage(report);
          setError("");
        }
      } catch (e) {
        if (alive) setError((e as Error).message);
      } finally {
        fetching = false;
      }
    }
    void load();
    const timer = setInterval(() => void load(), 4000);
    return () => {
      alive = false;
      clearInterval(timer);
    };
  }, [session, revision, insightsRevision, selected, refreshVersion]);
  async function refresh() {
    setBusy(true);
    try {
      await api.rpc("context.refresh", { session_id: session });
      setSelected("");
      setRefreshVersion((v) => v + 1);
    } catch (e) {
      setError((e as Error).message);
    } finally {
      setBusy(false);
    }
  }
  async function inspect(path: string) {
    if (!state?.snapshot) return;
    try {
      const before = await api.rpc<{ content: string }>("context.file", {
        session_id: session,
        snapshot_id: state.snapshot.id,
        path,
      });
      let after = "";
      try {
        after = (
          await api.rpc<{ content: string }>("files.read", {
            session_id: session,
            path,
          })
        ).content;
      } catch (e) {
        if (!/no such file|not found/i.test((e as Error).message)) throw e;
      }
      onPreview({ path, before: before.content, after });
    } catch (e) {
      setError((e as Error).message);
    }
  }
  const snapshot = state?.snapshot;
  const selection = snapshot?.selection;
  const ranks = new Map(selection?.ranks.map((r) => [r.path, r]));
  const calls = usage?.calls || [];
  const reported = (key: keyof TokenUsage) => {
    const known = calls
      .map((c) => c.metrics.usage?.[key])
      .filter((v): v is number => typeof v === "number");
    return {
      value: known.length ? known.reduce((a, b) => a + b, 0) : null,
      count: known.length,
    };
  };
  const input = reported("input_tokens");
  const output = reported("output_tokens");
  const cached = reported("cached_tokens");
  const files =
    snapshot?.files.filter((f) =>
      f.path.toLowerCase().includes(filter.toLowerCase()),
    ) || [];
  const run = usage?.runs[0];
  return (
    <div className="insights-panel">
      {error && (
        <p className="insights-error" role="alert">
          {error}
        </p>
      )}
      <section className="insights-section" aria-label="Repository context">
        <div className="insights-section-title">
          <span>REPOSITORY CONTEXT</span>
          <button
            className="icon-button"
            title={
              running
                ? "Snapshots update between model turns"
                : "Capture current workspace"
            }
            aria-label="Refresh context snapshot"
            disabled={!!running || busy}
            onClick={() => void refresh()}
          >
            <RefreshCw size={13} className={busy ? "spin" : ""} />
          </button>
        </div>
        {snapshot ? (
          <>
            <div className="context-version">
              <strong>Snapshot v{snapshot.revision}</strong>
              <span className={state.stale ? "context-stale" : "context-fresh"}>
                {state.stale ? "Workspace changed" : "Matches workspace"}
              </span>
            </div>
            <p className="insights-note">
              Checked {new Date(state.checked_at).toLocaleTimeString()}. Changes
              are checked between model turns and while this panel is open.
            </p>
            <select
              aria-label="Context snapshot version"
              value={selected}
              onChange={(e) => setSelected(e.target.value)}
            >
              <option value="">Latest snapshot</option>
              {history.map((v) => (
                <option value={v.id} key={v.id}>
                  v{v.revision} · {new Date(v.created_at).toLocaleString()}
                </option>
              ))}
            </select>
            <dl className="insights-stats">
              <div>
                <dt>Files included</dt>
                <dd>{snapshot.files.length}</dd>
              </div>
              <div>
                <dt>Source text</dt>
                <dd>{bytes(snapshot.source_bytes)}</dd>
              </div>
              <div>
                <dt>Repository payload</dt>
                <dd>{bytes(snapshot.prompt_bytes)}</dd>
              </div>
              <div>
                <dt>Payload budget</dt>
                <dd>{bytes(snapshot.budget_bytes)}</dd>
              </div>
            </dl>
            <progress
              aria-label="Repository context budget used"
              value={snapshot.prompt_bytes}
              max={snapshot.budget_bytes}
            />
            <p className="insights-hash" title={snapshot.digest}>
              SHA-256 {snapshot.digest.slice(0, 16)}…
              {snapshot.git_head && <> · Git {snapshot.git_head.slice(0, 7)}</>}
            </p>
            {selection && (
              <div className="run-summary" aria-label="Context selection">
                <strong>
                  {selection.strategy === "snap"
                    ? "Snap-assisted selection"
                    : selection.strategy === "snap_with_fallback"
                      ? "Snap + local selection"
                      : selection.strategy === "fallback"
                        ? "Local fallback selection"
                        : "Local task selection"}
                </strong>
                <span>
                  {selection.candidate_files} eligible files ·{" "}
                  {selection.snap_files} Snap scores used
                </span>
                {selection.reason && <span>{selection.reason}</span>}
                <span>
                  Automatic selection can use local ranking or send up to 128
                  task-ranked files to Snap. Scores for unchanged files can be
                  reused after an edit.
                </span>
              </div>
            )}
            {state.stale && (
              <p className="context-warning">
                {state.changed_paths.length
                  ? `${state.changed_paths.length} included path(s) changed. `
                  : "Workspace or context settings changed. "}
                Capture a new snapshot to update it. Pending edits still require
                a matching source version.
              </p>
            )}
            {snapshot.truncated && (
              <p className="context-warning">
                The scan reached its entry or byte limit or encountered
                inaccessible paths. Coverage is incomplete.
              </p>
            )}
            <input
              className="context-filter"
              aria-label="Filter context files"
              placeholder="Find a file in this snapshot…"
              value={filter}
              onChange={(e) => setFilter(e.target.value)}
            />
            <div className="context-files">
              {files.slice(0, 80).map((file) => (
                <button
                  key={file.path}
                  onClick={() => void inspect(file.path)}
                  title={`Compare snapshot ${file.hash} with the working file. ${ranks.has(file.path) ? `${ranks.get(file.path)!.source} relevance: ${ranks.get(file.path)!.score.toFixed(2)}/3` : ""}`}
                >
                  <FileCode2 size={12} />
                  <span>{file.path}</span>
                  <code>{file.hash.slice(0, 7)}</code>
                </button>
              ))}
              {files.length > 80 && (
                <p className="insights-note">
                  Showing 80 of {files.length} paths. Use the filter to narrow
                  the list.
                </p>
              )}
            </div>
            <details className="context-exclusions">
              <summary>
                Exclusions and coverage · {snapshot.excluded.length} recorded
              </summary>
              <p className="insights-note">
                Git ignore rules, generated folders, symlinks, sensitive paths,
                private key headers, binary files, and files over 128 KiB are
                excluded. Ignored descendants are not enumerated. At most 2,000
                candidate files, 32 MiB of reads and 500 exclusion details are
                recorded. Selection ranks full eligible files within the payload
                budget; small applicable AGENTS.md files are kept first.
              </p>
              {snapshot.excluded.map((entry, i) => {
                const rank = ranks.get(entry.path);
                const shortlist =
                  selection?.model && rank?.local_rank
                    ? rank.snap_candidate
                      ? "On Snap shortlist"
                      : "Outside Snap shortlist"
                    : "";
                const details = [
                  rank?.local_rank && `Local rank ${rank.local_rank}`,
                  shortlist,
                ]
                  .filter(Boolean)
                  .join(" · ");
                return (
                  <div key={`${entry.path}-${i}`} title={details || undefined}>
                    <code>{entry.path}</code>
                    <span>{entry.reason}</span>
                  </div>
                );
              })}
            </details>
          </>
        ) : (
          <div className="insights-note">
            No repository snapshot yet. Capture the workspace to begin.
          </div>
        )}
      </section>
      <section className="insights-section" aria-label="Model usage">
        <div className="insights-section-title">
          <span>MODEL USAGE</span>
          <Activity size={13} />
        </div>
        <p className="insights-note">
          {usage?.total_calls || 0} model call(s).{" "}
          {usage && usage.total_calls > calls.length
            ? `Showing the latest ${calls.length}. `
            : ""}
          Only provider-reported counts are totaled.{" "}
          {calls.filter((c) => c.kind === "context_selection").length} Snap
          selection call(s) in this window.
        </p>
        <dl className="insights-stats usage-totals">
          <div>
            <dt>
              Input tokens{" "}
              <small>
                {input.count}/{calls.length} calls
              </small>
            </dt>
            <dd>{number(input.value)}</dd>
          </div>
          <div>
            <dt>
              Output tokens{" "}
              <small>
                {output.count}/{calls.length} calls
              </small>
            </dt>
            <dd>{number(output.value)}</dd>
          </div>
          <div>
            <dt>
              Cached tokens{" "}
              <small>
                {cached.count}/{calls.length} calls
              </small>
            </dt>
            <dd>{number(cached.value)}</dd>
          </div>
        </dl>
        <p className="insights-note">
          Missing usage is unknown. Estimates use request bytes ÷ 4, not the
          model tokenizer. Cost is unavailable; no pricing or cache discounts
          are assumed.
        </p>
        {run && (
          <div className="run-summary">
            <strong>Latest task · {run.status}</strong>
            <span>
              {duration(
                run.ended_at == null ? null : run.ended_at - run.started_at,
              )}{" "}
              elapsed · {run.tool_calls} tool calls
            </span>
            <span>
              {duration(run.tool_ms)} tool execution ·{" "}
              {duration(run.approval_wait_ms)} approval wait
            </span>
            {!!run.context_reads?.successful && (
              <span>
                {run.context_reads.missed} of {run.context_reads.successful}{" "}
                file reads were outside the snapshot
              </span>
            )}
            {!!run.context_reads?.missed_paths.length && (
              <details>
                <summary>Files read outside the snapshot</summary>
                <ul>
                  {run.context_reads.missed_paths.map((miss) => (
                    <li key={miss.path}>
                      <code>{miss.path}</code>
                      {!miss.eligible && " · outside the candidate inventory"}
                    </li>
                  ))}
                </ul>
              </details>
            )}
          </div>
        )}
        {!calls.length && (
          <p className="insights-note">
            Call traces appear when you send a task.
          </p>
        )}
        {calls.slice(0, 30).map((call, index) => (
          <div className="call-trace" key={call.id}>
            <button
              onClick={() => setExpanded(expanded === call.id ? null : call.id)}
              aria-expanded={expanded === call.id}
            >
              <span className={`trace-dot trace-${call.status}`} />
              <span>
                Call {usage!.total_calls - index} ·{" "}
                {call.kind === "context_selection"
                  ? "Snap selection"
                  : `v${call.context_revision}`}
              </span>
              <span>{duration(call.metrics.duration_ms)}</span>
              <ChevronDown size={12} />
            </button>
            {expanded === call.id && (
              <dl className="insights-stats">
                <div>
                  <dt>Model / purpose</dt>
                  <dd>
                    {call.model} ·{" "}
                    {call.kind === "context_selection"
                      ? "Context selection"
                      : "Generation"}
                  </dd>
                </div>
                <div>
                  <dt>Status</dt>
                  <dd>
                    {call.status}
                    {call.metrics.http_status
                      ? ` · HTTP ${call.metrics.http_status}`
                      : ""}
                  </dd>
                </div>
                <div>
                  <dt>First output</dt>
                  <dd>{duration(call.metrics.first_output_ms)}</dd>
                </div>
                <div>
                  <dt>Request / response</dt>
                  <dd>
                    {bytes(call.metrics.request_bytes)} /{" "}
                    {bytes(call.metrics.response_bytes)}
                  </dd>
                </div>
                <div>
                  <dt>Context / history</dt>
                  <dd>
                    {bytes(call.context_bytes)} / {bytes(call.history_bytes)}
                    {call.history_stored_bytes != null && (
                      <> sent of {bytes(call.history_stored_bytes)} stored</>
                    )}
                  </dd>
                </div>
                <div>
                  <dt>Estimated input</dt>
                  <dd>~{number(call.estimated_input_tokens)} tokens</dd>
                </div>
                <div>
                  <dt>Reported input / output</dt>
                  <dd>
                    {number(call.metrics.usage?.input_tokens)} /{" "}
                    {number(call.metrics.usage?.output_tokens)}
                  </dd>
                </div>
                <div>
                  <dt>Reported reasoning</dt>
                  <dd>{number(call.metrics.usage?.reasoning_tokens)}</dd>
                </div>
                <div>
                  <dt>Finish / tools requested</dt>
                  <dd>
                    {call.metrics.finish_reason || "—"} / {call.tool_calls}
                  </dd>
                </div>
              </dl>
            )}
          </div>
        ))}
      </section>
    </div>
  );
}
