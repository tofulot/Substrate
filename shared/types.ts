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
