import { create } from "zustand";
import type {
  AgentEvent,
  Config,
  Message,
  Project,
  Session,
  ToolActivity,
} from "../../shared/types";
export const api = window.substrate;
interface State {
  project: Project | null;
  session: Session | null;
  sessions: Session[];
  config: Config | null;
  running: string | null;
  error: string | null;
  connected: boolean;
  settings: boolean;
  revision: number;
  terminalOutput: string;
  init(): Promise<void>;
  openProject(): Promise<void>;
  selectSession(id: string): Promise<void>;
  newSession(): Promise<void>;
  send(task: string): Promise<void>;
  event(event: AgentEvent): void;
  refreshSessions(): Promise<void>;
  setError(error: unknown): void;
}
export const useStore = create<State>((set, get) => ({
  project: null,
  session: null,
  sessions: [],
  config: null,
  running: null,
  error: null,
  connected: false,
  settings: false,
  revision: 0,
  terminalOutput: "",
  setError: (error) =>
    set({ error: error instanceof Error ? error.message : String(error) }),
  init: async () => {
    try {
      if (!api)
        throw new Error(
          "Open Substrate in the desktop app to connect to the Rust harness.",
        );
      const [, config, sessions] = await Promise.all([
        api.rpc("ping"),
        api.rpc<Config>("config.get"),
        api.rpc<Session[]>("session.list"),
      ]);
      set({ connected: true, config, sessions });
      const last = localStorage.getItem("substrate.session");
      if (last && sessions.some((s) => s.id === last))
        await get().selectSession(last);
    } catch (error) {
      get().setError(error);
    }
  },
  refreshSessions: async () => {
    set({ sessions: await api.rpc<Session[]>("session.list") });
  },
  openProject: async () => {
    try {
      if (get().running)
        throw new Error("Stop the current task before switching workspaces.");
      const project = await api.openProject();
      if (!project) return;
      set({
        project,
        error: null,
        terminalOutput: "",
        revision: get().revision + 1,
      });
      const existing = get().sessions.find((s) => s.project === project.path);
      if (existing) await get().selectSession(existing.id);
      else await get().newSession();
    } catch (error) {
      get().setError(error);
    }
  },
  selectSession: async (id) => {
    try {
      if (get().running && get().running !== id)
        throw new Error(
          "Stop the current task before switching conversations.",
        );
      const [session, branch] = await Promise.all([
        api.rpc<Session>("session.resume", { session_id: id }),
        api.rpc<string | null>("git.branch", { session_id: id }),
      ]);
      // Project identity is restored from the database, never from an arbitrary renderer path.
      const path = session.project;
      const name = path.split(/[\\/]/).pop() || "Workspace";
      set({
        session,
        project: { path, name, branch },
        running: session.running ? id : null,
        error: null,
        terminalOutput: "",
        revision: get().revision + 1,
      });
      localStorage.setItem("substrate.session", id);
    } catch (error) {
      get().setError(error);
    }
  },
  newSession: async () => {
    try {
      if (get().running)
        throw new Error(
          "Stop the current task before creating a conversation.",
        );
      const project = get().project;
      if (!project) {
        await get().openProject();
        return;
      }
      const session = await api.rpc<Session>("session.create", {
        project: project.path,
      });
      set({ session, error: null, terminalOutput: "" });
      localStorage.setItem("substrate.session", session.id);
      await get().refreshSessions();
    } catch (error) {
      get().setError(error);
    }
  },
  send: async (task) => {
    const { session, running, config } = get();
    if (running || !task.trim()) return;
    if (!config?.has_key) {
      set({ settings: true });
      return;
    }
    if (!session) {
      await get().openProject();
      return;
    }
    set({
      running: session.id,
      error: null,
      session: {
        ...session,
        title: session.messages.length ? session.title : task.slice(0, 60),
        messages: [
          ...session.messages,
          { id: crypto.randomUUID(), role: "user", content: task },
        ],
      },
    });
    try {
      await api.rpc("agent.start", { session_id: session.id, task });
    } catch (error) {
      set({ running: null, session });
      get().setError(error);
    }
  },
  event: (event) => {
    const { method, params } = event;
    if (method === "connection.closed") {
      set({
        connected: false,
        running: null,
        error: params.message || "Harness disconnected",
      });
      return;
    }
    if (params.session_id !== get().session?.id) return;
    if (method === "agent.message.delta") {
      set((state) => {
        const session = state.session!;
        const messages = [...session.messages];
        const index = messages.findIndex((m) => m.id === params.message_id);
        if (index >= 0)
          messages[index] = {
            ...messages[index],
            content: messages[index].content + params.delta,
          };
        else
          messages.push({
            id: params.message_id!,
            role: "assistant",
            content: params.delta || "",
          });
        return { session: { ...session, messages } };
      });
    } else if (method.startsWith("tool.") && params.tool) {
      const tool = params.tool as ToolActivity;
      set((state) => {
        const session = state.session!;
        const messages: Message[] = [...session.messages];
        const index = messages.findIndex((m) => m.tool?.id === tool.id);
        const message: Message = {
          id: tool.id,
          role: "tool",
          content: "",
          tool,
        };
        if (index >= 0) messages[index] = message;
        else messages.push(message);
        return {
          session: { ...session, messages },
          revision: state.revision + (method === "tool.completed" ? 1 : 0),
        };
      });
    } else if (method === "tool.output.delta") {
      set((s) => ({
        terminalOutput: (s.terminalOutput + (params.delta || "")).slice(
          -100000,
        ),
      }));
    } else if (
      ["agent.completed", "agent.failed", "agent.interrupted"].includes(method)
    ) {
      const id = get().session!.id;
      set((s) => ({
        running: null,
        error: method === "agent.failed" ? params.message : null,
        revision: s.revision + 1,
      }));
      // Reload authoritative persisted messages, including final tool statuses.
      void api
        .rpc<Session>("session.resume", { session_id: id })
        .then((session) => {
          if (get().session?.id === id && !get().running) set({ session });
        })
        .catch(get().setError);
      void get().refreshSessions().catch(get().setError);
    }
  },
}));
