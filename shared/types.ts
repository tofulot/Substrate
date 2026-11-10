export interface Project {
  path: string;
  name: string;
  branch: string | null;
}
export interface ToolActivity {
  id: string;
  name: string;
  arguments: Record<string, unknown>;
  status: "running" | "approval" | "completed" | "failed" | "rejected";
  output?: string;
  preview?: { path: string; before: string; after: string };
}
export interface Message {
  id: string;
  role: "user" | "assistant" | "tool";
  content: string;
  tool?: ToolActivity;
}
export interface Session {
  running?: boolean;
  id: string;
  project: string;
  title: string;
  updated_at: number;
  messages: Message[];
}
export interface FileEntry {
  path: string;
  name: string;
  directory: boolean;
}
export interface Change {
  path: string;
  status: string;
}
export interface Config {
  model: string;
  has_key: boolean;
  max_steps: number;
  context_budget_bytes: number;
}
export interface AgentEvent {
  method: string;
  params: {
    session_id?: string;
    delta?: string;
    message_id?: string;
    message?: string;
    tool?: ToolActivity;
    [key: string]: unknown;
  };
}
export interface DesktopAPI {
  rpc<T = unknown>(
    method: string,
    params?: Record<string, unknown>,
  ): Promise<T>;
  openProject(): Promise<Project | null>;
  setApiKey(key: string): Promise<void>;
  onEvent(callback: (event: AgentEvent) => void): () => void;
  platform: string;
}
declare global {
  interface Window {
    substrate: DesktopAPI;
  }
}

export interface FileVersion {
  path: string;
  hash: string;
  bytes: number;
}
export interface ContextChange {
  path: string;
  kind: "added" | "modified" | "removed";
  previous_hash: string | null;
  hash: string | null;
}
export interface ContextSnapshot {
  id: string;
  session_id: string;
  revision: number;
  parent_id: string | null;
  digest: string;
  created_at: number;
  git_head: string | null;
  budget_bytes: number;
  source_bytes: number;
  prompt_bytes: number;
  scanned_files: number;
  truncated: boolean;
  files: FileVersion[];
  excluded: { path: string; reason: string }[];
  changes: ContextChange[];
  selection?: ContextSelection | null;
}
export interface ContextSelection {
  strategy: "local" | "snap" | "snap_with_fallback" | "fallback";
  model: string | null;
  task_hash: string;
  inventory_digest: string;
  candidate_files: number;
  snap_files: number;
  reason: string | null;
  ranks: {
    path: string;
    hash: string;
    score: number;
    confidence: number | null;
    source: string;
  }[];
}
export interface ContextState {
  snapshot: ContextSnapshot | null;
  stale: boolean;
  changed_paths: string[];
  checked_at: number;
}
export interface TokenUsage {
  input_tokens: number | null;
  output_tokens: number | null;
  total_tokens: number | null;
  cached_tokens: number | null;
  reasoning_tokens: number | null;
}
export interface CallTrace {
  id: string;
  run_id: string;
  session_id: string;
  model: string;
  kind?: "generation" | "context_selection";
  started_at: number;
  status: string;
  snapshot_id: string;
  context_revision: number;
  context_bytes: number;
  history_bytes: number;
  estimated_input_tokens: number | null;
  tool_calls: number;
  metrics: {
    request_bytes: number | null;
    response_bytes: number | null;
    duration_ms: number | null;
    first_output_ms: number | null;
    http_status: number | null;
    finish_reason: string | null;
    usage: TokenUsage | null;
  };
}
export interface RunTrace {
  id: string;
  session_id: string;
  model: string;
  started_at: number;
  ended_at: number | null;
  status: string;
  tool_calls: number;
  tool_ms: number;
  approval_wait_ms: number;
}
export interface UsageReport {
  calls: CallTrace[];
  runs: RunTrace[];
  total_calls: number;
}
