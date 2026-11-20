import { create } from "zustand";
import type {
  AgentEvent,
  ApprovalMode,
  Config,
  Message,
  Project,
  Session,
  ToolActivity,
} from "../../shared/types";
import { workspaceName } from "./utils";
export const api = window.substrate;
interface State {
  project: Project | null;
  session: Session | null;
  sessions: Session[];
  config: Config | null;
  running: string | null;
  starting: boolean;
  approvalMode: ApprovalMode;
  changingApprovalMode: boolean;
  navigating: boolean;
  error: string | null;
  connected: boolean;
  settings: boolean;
  revision: number;
  insightsRevision: number;
  terminalOutput: string;
  init(): Promise<void>;
  openProject(): Promise<void>;
  createProject(name: string): Promise<void>;
  selectSession(id: string): Promise<void>;
  newSession(): Promise<void>;
  send(task: string): Promise<void>;
  setApprovalMode(mode: ApprovalMode): Promise<void>;
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
  starting: false,
  approvalMode: "ask",
  changingApprovalMode: false,
  navigating: false,
  error: null,
  connected: false,
  settings: false,
  revision: 0,
  insightsRevision: 0,
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
      if (last && !get().navigating && sessions.some((s) => s.id === last))
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
      await navigate(() => switchProject(() => api.openProject()));
    } catch (error) {
      get().setError(error);
    }
  },
  createProject: (name) =>
    navigate(() => switchProject(() => api.createProject(name))),
  selectSession: async (id) => {
    if (get().session?.id === id) return;
    try {
      await navigate(() => resumeSession(id));
    } catch (error) {
      get().setError(error);
    }
  },
  newSession: async () => {
    try {
      const project = get().project;
      await navigate(() =>
        project
          ? createSession(project)
          : switchProject(() => api.openProject()),
      );
    } catch (error) {
      get().setError(error);
    }
  },
  send: async (task) => {
    const {
      session,
      running,
      starting,
      navigating,
      config,
      approvalMode,
      changingApprovalMode,
    } = get();
    if (
      running ||
      starting ||
      navigating ||
      changingApprovalMode ||
      !task.trim()
    )
      return;
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
      starting: true,
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
      await api.rpc("agent.start", {
        session_id: session.id,
        task,
        approval_mode: approvalMode,
      });
    } catch (error) {
      set({ running: null, session });
      get().setError(error);
    } finally {
      set({ starting: false });
    }
  },
  setApprovalMode: async (mode) => {
    const { running, starting, changingApprovalMode } = get();
    if (starting || changingApprovalMode) return;
    if (!running) {
      set({ approvalMode: mode });
      return;
    }
    set({ changingApprovalMode: true });
    try {
      await api.rpc("agent.set_approval_mode", {
        session_id: running,
        approval_mode: mode,
      });
      set({ approvalMode: mode });
    } catch (error) {
      get().setError(error);
    } finally {
      set({ changingApprovalMode: false });
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
    if (method === "context.updated" || method === "usage.updated") {
      set((s) => ({ insightsRevision: s.insightsRevision + 1 }));
    } else if (method === "agent.message.delta") {
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

async function switchProject(load: () => Promise<Project | null>) {
  const project = await load();
  if (!project) return;
  const existing = useStore
    .getState()
    .sessions.find((s) => s.project === project.path);
  if (existing) await resumeSession(existing.id, project);
  else await createSession(project);
}

async function navigate(load: () => Promise<void>) {
  const { navigating, running } = useStore.getState();
  if (running)
    throw new Error(
      "Stop the current task before switching conversations or workspaces.",
    );
  if (navigating) throw new Error("A conversation is already opening.");
  useStore.setState({ navigating: true });
  try {
    await load();
  } finally {
    useStore.setState({ navigating: false });
  }
}

async function resumeSession(id: string, project?: Project) {
  // The saved conversation remains readable if its folder has moved or been deleted.
  const [session, branch] = await Promise.all([
    api.rpc<Session>("session.resume", { session_id: id }),
    project
      ? project.branch
      : api
          .rpc<string | null>("git.branch", { session_id: id })
          .catch(() => null),
  ]);
  activateSession(session, branch);
}

async function createSession(project: Project) {
  const session = await api.rpc<Session>("session.create", {
    project: project.path,
  });
  activateSession(session, project.branch);
  await useStore.getState().refreshSessions();
}

function activateSession(session: Session, branch: string | null) {
  const path = session.project;
  useStore.setState((state) => ({
    session,
    project: { path, name: workspaceName(path), branch },
    running: session.running ? session.id : null,
    approvalMode: session.approval_mode ?? state.approvalMode,
    error: null,
    terminalOutput: "",
    revision: state.revision + 1,
  }));
  localStorage.setItem("substrate.session", session.id);
}
