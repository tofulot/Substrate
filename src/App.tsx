import { lazy, Suspense, useEffect, useMemo, useRef, useState } from "react";
import * as Dialog from "@radix-ui/react-dialog";
import {
  Activity,
  ArrowDown,
  ArrowRight,
  ArrowUp,
  Check,
  ChevronDown,
  ChevronRight,
  Circle,
  CircleDot,
  Code2,
  Command,
  File,
  FileCode2,
  Files,
  Folder,
  FolderOpen,
  FolderPlus,
  GitBranch,
  GitCompareArrows,
  Layers3,
  Loader2,
  MessageSquare,
  PanelLeftClose,
  PanelLeftOpen,
  PanelRight,
  Plus,
  Search,
  Settings2,
  ShieldCheck,
  Sparkles,
  Square,
  TerminalSquare,
  X,
} from "lucide-react";
import Markdown from "react-markdown";
import { version as appVersion } from "../package.json";
import type { ApprovalMode, Session } from "../shared/types";
import { api, useStore } from "./lib/store";
import { workspaceName } from "./lib/utils";
import { Button } from "./components/ui/button";
import Settings from "./components/Settings";
import type { Change, FileEntry, Preview, ToolActivity } from "./view-types";
const CodeView = lazy(() => import("./components/CodeView"));
const Insights = lazy(() => import("./components/Insights"));
const Terminal = lazy(() => import("./components/Terminal"));
function Mark({ small = false }: { small?: boolean }) {
  return (
    <svg
      className={small ? "mark mark-small" : "mark"}
      width="40"
      height="44"
      viewBox="0 0 40 44"
      fill="none"
      aria-hidden="true"
    >
      <path d="M4 13 20 4l16 9-16 9L4 13Z" />
      <path d="m4 22 16 9 16-9M4 31l16 9 16-9" />
    </svg>
  );
}
const toolNames: Record<string, string> = {
  read_file: "Read file",
  list_files: "Explore files",
  search_repository: "Search repository",
  web_search: "Search the web",
  fetch_url: "Fetch web page",
  apply_patch: "Edit file",
  execute_command: "Run command",
  run_check: "Run verification check",
  verify_npm_app: "Verify app launch",
  git_diff: "Inspect changes",
};
function ToolCard({
  tool,
  onReview,
  onPreview,
}: {
  tool: ToolActivity;
  onReview: (tool: ToolActivity) => void;
  onPreview: (preview: Preview) => void;
}) {
  const [expanded, setExpanded] = useState(false);
  const icon =
    tool.status === "approval" ? (
      <ShieldCheck size={15} />
    ) : tool.status === "running" ? (
      <Loader2 size={15} className="spin" />
    ) : tool.status === "completed" ? (
      <Check size={15} />
    ) : (
      <X size={15} />
    );
  return (
    <div className={`tool-card tool-${tool.status}`}>
      <button
        className="tool-summary"
        onClick={() => setExpanded(!expanded)}
        aria-expanded={expanded}
      >
        <span className="tool-icon">{icon}</span>
        <span className="tool-label">{toolNames[tool.name] || tool.name}</span>
        <code>
          {String(
            tool.arguments.path ||
              tool.arguments.query ||
              tool.arguments.url ||
              tool.arguments.command ||
              "",
          ).slice(0, 75)}
        </code>
        <ChevronDown size={13} className={expanded ? "rotate" : ""} />
      </button>
      {expanded && (
        <pre className="tool-detail">
          {tool.output || JSON.stringify(tool.arguments, null, 2)}
        </pre>
      )}
      {tool.status === "approval" && (
        <div className="tool-approval">
          <span>
            {tool.name === "apply_patch"
              ? "A proposed edit is ready for your review."
              : "Permission needed to run on your machine."}
          </span>
          <Button size="sm" onClick={() => onReview(tool)}>
            Review <ArrowRight size={13} />
          </Button>
        </div>
      )}
      {tool.preview && tool.status !== "approval" && (
        <button
          className="tool-diff-link"
          onClick={() => onPreview(tool.preview!)}
        >
          <GitCompareArrows size={13} /> View diff <ArrowRight size={12} />
        </button>
      )}
    </div>
  );
}
function FileTree({
  parent = "",
  depth = 0,
  onOpen,
}: {
  parent?: string;
  depth?: number;
  onOpen: (path: string) => void;
}) {
  const sessionId = useStore((s) => s.session?.id);
  const revision = useStore((s) => s.revision);
  const [entries, setEntries] = useState<FileEntry[]>([]);
  const [folders, setFolders] = useState<Set<string>>(new Set());
  const [error, setError] = useState("");
  useEffect(() => {
    if (!sessionId) return;
    let live = true;
    void api
      .rpc<FileEntry[]>("files.list", { session_id: sessionId, path: parent })
      .then((data) => {
        if (live) {
          setEntries(data);
          setError("");
        }
      })
      .catch((e) => {
        if (live) setError(e.message);
      });
    return () => {
      live = false;
    };
  }, [sessionId, parent, revision]);
  if (error) return <p className="tree-error">{error}</p>;
  return (
    <div>
      {entries.map((entry) => (
        <div key={entry.path}>
          <button
            className="file-row"
            style={{ paddingLeft: 18 + depth * 14 }}
            onClick={() =>
              entry.directory
                ? setFolders((previous) => {
                    const next = new Set(previous);
                    if (next.has(entry.path)) next.delete(entry.path);
                    else next.add(entry.path);
                    return next;
                  })
                : onOpen(entry.path)
            }
          >
            <span className="file-chevron">
              {entry.directory &&
                (folders.has(entry.path) ? (
                  <ChevronDown size={12} />
                ) : (
                  <ChevronRight size={12} />
                ))}
            </span>
            {entry.directory ? <Folder size={15} /> : <FileCode2 size={14} />}
            <span>{entry.name}</span>
          </button>
          {entry.directory && folders.has(entry.path) && (
            <FileTree parent={entry.path} depth={depth + 1} onOpen={onOpen} />
          )}
        </div>
      ))}
    </div>
  );
}
function NewWorkspaceButton({ disabled }: { disabled: boolean }) {
  const [open, setOpen] = useState(false);
  const [name, setName] = useState("");
  const [error, setError] = useState("");
  const [creating, setCreating] = useState(false);

  return (
    <Dialog.Root
      open={open}
      onOpenChange={(next) => {
        if (creating) return;
        setOpen(next);
        setName("");
        setError("");
      }}
    >
      <Dialog.Trigger className="new-workspace-button" disabled={disabled}>
        <FolderPlus size={14} /> New folder
      </Dialog.Trigger>
      <Dialog.Portal>
        <Dialog.Overlay className="modal-overlay" />
        <Dialog.Content className="settings-modal">
          <Dialog.Title>New workspace folder</Dialog.Title>
          <Dialog.Description>
            Create and open a workspace in Documents/Substrate/Projects.
          </Dialog.Description>
          <form
            onSubmit={async (event) => {
              event.preventDefault();
              if (creating || !name.trim()) return;
              setCreating(true);
              setError("");
              try {
                await useStore.getState().createProject(name);
                setOpen(false);
              } catch (error) {
                const message =
                  error instanceof Error ? error.message : String(error);
                setError(
                  message.replace(
                    /^Error invoking remote method 'project:create': Error: /u,
                    "",
                  ),
                );
              } finally {
                setCreating(false);
              }
            }}
          >
            <label htmlFor="workspace-folder-name">Folder name</label>
            <div className="key-field">
              <input
                id="workspace-folder-name"
                value={name}
                onChange={(event) => setName(event.target.value)}
                readOnly={creating}
                required
              />
            </div>
            {error && (
              <p className="form-error" role="alert">
                {error}
              </p>
            )}
            <div className="modal-actions">
              <Dialog.Close asChild>
                <Button variant="outline" type="button" disabled={creating}>
                  Cancel
                </Button>
              </Dialog.Close>
              <Button type="submit" disabled={creating || !name.trim()}>
                {creating ? "Creating…" : "Create folder"}
              </Button>
            </div>
          </form>
        </Dialog.Content>
      </Dialog.Portal>
    </Dialog.Root>
  );
}

export default function App() {
  const {
    project,
    session,
    sessions,
    config,
    connected,
    running,
    starting,
    approvalMode,
    changingApprovalMode,
    setApprovalMode,
    navigating,
    error,
    revision,
  } = useStore();
  const [drafts, setDrafts] = useState<Record<string, string>>({});
  const draftKey = session?.id ?? "";
  const draft = drafts[draftKey] ?? "";
  function setDraft(value: string) {
    setDrafts((current) => ({ ...current, [draftKey]: value }));
  }
  const [sidebar, setSidebar] = useState(true);
  const [panel, setPanel] = useState(false);
  const [tab, setTab] = useState<"files" | "changes" | "insights">("files");
  const [terminal, setTerminal] = useState(false);
  const [selected, setSelected] = useState<Preview | null>(null);
  const [fileContent, setFileContent] = useState<string | null>(null);
  const [filePath, setFilePath] = useState("");
  const [changes, setChanges] = useState<Change[]>([]);
  const [gitError, setGitError] = useState("");
  const [approval, setApproval] = useState<ToolActivity | null>(null);
  const [approving, setApproving] = useState(false);
  const [query, setQuery] = useState("");
  const [searching, setSearching] = useState(false);
  const [collapsedProjects, setCollapsedProjects] = useState(
    () => new Set<string>(),
  );
  const scroll = useRef<HTMLDivElement>(null);
  const input = useRef<HTMLTextAreaElement>(null);
  const stick = useRef(true);
  const currentPending = session?.messages.find(
    (m) => m.tool?.status === "approval",
  )?.tool;
  const activeApproval = approval
    ? session?.messages.find((m) => m.tool?.id === approval.id)?.tool
    : undefined;
  useEffect(() => {
    const off = api?.onEvent((e) => useStore.getState().event(e));
    void useStore.getState().init();
    return off;
  }, []);
  useEffect(() => {
    stick.current = true;
    if (session) {
      const path = session.project;
      setCollapsedProjects((current) => {
        if (!current.has(path)) return current;
        const next = new Set(current);
        next.delete(path);
        return next;
      });
    }
  }, [session?.id, session?.project]);
  useEffect(() => {
    if (!draftKey) return;
    setDrafts((current) => {
      const { "": welcomeDraft, ...chatDrafts } = current;
      if (!welcomeDraft) return current;
      return {
        ...chatDrafts,
        [draftKey]: chatDrafts[draftKey] ?? welcomeDraft,
      };
    });
  }, [draftKey]);
  useEffect(() => {
    function shortcuts(event: KeyboardEvent) {
      if (event.metaKey || event.ctrlKey) {
        if (event.key === ",") {
          event.preventDefault();
          useStore.setState({ settings: !useStore.getState().settings });
        }
        if (event.key === "n") {
          event.preventDefault();
          void useStore.getState().newSession();
        }
        if (event.key === "o") {
          event.preventDefault();
          void useStore.getState().openProject();
        }
        if (event.key === "b") {
          event.preventDefault();
          setSidebar((s) => !s);
        }
      }
      if (
        event.key === "Escape" &&
        useStore.getState().running &&
        !document.querySelector('[role="dialog"]')
      )
        void api
          .rpc("agent.interrupt", { session_id: useStore.getState().running })
          .catch(useStore.getState().setError);
    }
    document.addEventListener("keydown", shortcuts, true);
    return () => document.removeEventListener("keydown", shortcuts, true);
  }, []);
  useEffect(() => {
    if (stick.current)
      scroll.current?.scrollTo({
        top: scroll.current.scrollHeight,
        behavior: "smooth",
      });
  }, [session?.messages, running]);
  useEffect(() => {
    setSelected(null);
    setFileContent(null);
    setFilePath("");
    setChanges([]);
    setApproval(null);
  }, [session?.id]);
  useEffect(() => {
    if (!session) return;
    let live = true;
    void api
      .rpc<Change[]>("git.status", { session_id: session.id })
      .then((data) => {
        if (live) {
          setChanges(data);
          setGitError("");
        }
      })
      .catch((e) => {
        if (live) setGitError(e.message);
      });
    return () => {
      live = false;
    };
  }, [session?.id, revision]);
  const fileIsOpen = fileContent !== null;
  useEffect(() => {
    if (!session || !filePath || !fileIsOpen) return;
    let live = true;
    void api
      .rpc<{ content: string }>("files.read", {
        session_id: session.id,
        path: filePath,
      })
      .then((result) => {
        if (live) setFileContent(result.content);
      })
      .catch((e) => {
        if (live) useStore.getState().setError(e);
      });
    return () => {
      live = false;
    };
  }, [session?.id, revision, filePath, fileIsOpen]);
  async function openFile(path: string, diff = false) {
    if (!session) return;
    try {
      if (diff) {
        setSelected(
          await api.rpc<Preview>("git.file", { session_id: session.id, path }),
        );
        setFileContent(null);
      } else {
        const result = await api.rpc<{ content: string }>("files.read", {
          session_id: session.id,
          path,
        });
        setFileContent(result.content);
        setFilePath(path);
        setSelected(null);
      }
      setPanel(true);
    } catch (e) {
      useStore.getState().setError(e);
    }
  }
  async function decide(approved: boolean) {
    if (!activeApproval || !session) return;
    setApproving(true);
    try {
      await api.rpc(approved ? "tool.approve" : "tool.reject", {
        session_id: session.id,
        tool_id: activeApproval.id,
      });
      setApproval(null);
    } catch (e) {
      useStore.getState().setError(e);
    } finally {
      setApproving(false);
    }
  }
  function submit() {
    if (
      !draft.trim() ||
      running ||
      starting ||
      navigating ||
      changingApprovalMode
    )
      return;
    if (!config?.has_key) {
      useStore.setState({ settings: true });
      return;
    }
    if (!session) {
      void useStore.getState().openProject();
      return;
    }
    const task = draft.trim();
    setDraft("");
    stick.current = true;
    void useStore.getState().send(task);
  }
  const searchTerm = query.trim().toLowerCase();
  const projectGroups = useMemo(() => {
    const groups = new Map<string, Session[]>();
    for (const chat of sessions) {
      if (
        !chat.project.toLowerCase().includes(searchTerm) &&
        !chat.title.toLowerCase().includes(searchTerm)
      )
        continue;
      const group = groups.get(chat.project);
      if (group) group.push(chat);
      else groups.set(chat.project, [chat]);
    }
    return [...groups];
  }, [sessions, searchTerm]);
  const navigationDisabled = !connected || !!running || navigating;
  const hasMessages = !!session?.messages.length;
  return (
    <div
      className={`app ${sidebar ? "" : "sidebar-hidden"} ${panel ? "with-panel" : ""}`}
    >
      <aside className="sidebar">
        <div className="sidebar-titlebar">
          <button
            className="icon-button sidebar-toggle"
            title="Hide sidebar (⌘B)"
            onClick={() => setSidebar(false)}
            aria-label="Hide sidebar"
          >
            <PanelLeftClose size={17} />
          </button>
        </div>
        <div className="brand">
          <Mark small />
          <span>
            substrate<span className="brand-dot">.</span>
          </span>
          <span className="alpha-tag">ALPHA</span>
        </div>
        <div className="sidebar-actions">
          <button
            className="new-chat"
            disabled={navigationDisabled}
            onClick={() => void useStore.getState().newSession()}
          >
            <Plus size={17} />
            <span>New conversation</span>
            <kbd>⌘ N</kbd>
          </button>
          <button
            className="search-button"
            onClick={() => {
              setSearching(!searching);
              setQuery("");
            }}
            aria-expanded={searching}
            aria-label="Search chat history"
            title="Search chat history"
          >
            <Search size={16} />
          </button>
        </div>
        {searching && (
          <input
            className="conversation-search"
            autoFocus
            type="search"
            aria-label="Search by project or chat title"
            placeholder="Find a project or chat…"
            value={query}
            onChange={(e) => {
              setQuery(e.target.value);
              if (collapsedProjects.size) setCollapsedProjects(new Set());
            }}
          />
        )}
        <div className="sidebar-section-label">
          WORKSPACE
          <NewWorkspaceButton disabled={navigationDisabled} />
        </div>
        <button
          className={`workspace-card ${project ? "selected" : ""}`}
          disabled={navigationDisabled}
          onClick={() => void useStore.getState().openProject()}
          title="Open workspace (⌘O)"
        >
          <span className="workspace-icon">
            <FolderOpen size={18} />
          </span>
          <span>
            <strong>{project?.name || "Open a workspace"}</strong>
            <small>{project ? "Local folder" : "Bring your code along"}</small>
          </span>
          <ChevronDown size={13} />
        </button>
        <div className="sidebar-section-label" id="chat-history-label">
          CHAT HISTORY
        </div>
        <nav className="conversation-list" aria-labelledby="chat-history-label">
          {projectGroups.length ? (
            <ul>
              {projectGroups.map(([path, chats]) => {
                const name = workspaceName(path);
                const expanded = !collapsedProjects.has(path);
                return (
                  <li className="history-project" key={path}>
                    <button
                      className={`history-project-toggle ${path === project?.path ? "active" : ""}`}
                      aria-expanded={expanded}
                      title={path}
                      onClick={() =>
                        setCollapsedProjects((current) => {
                          const next = new Set(current);
                          if (next.has(path)) next.delete(path);
                          else next.add(path);
                          return next;
                        })
                      }
                    >
                      {expanded ? (
                        <ChevronDown size={12} />
                      ) : (
                        <ChevronRight size={12} />
                      )}
                      {expanded ? (
                        <FolderOpen size={15} />
                      ) : (
                        <Folder size={15} />
                      )}
                      <span>{name}</span>
                    </button>
                    {expanded && (
                      <ul
                        className="history-chats"
                        aria-label={`${name} chats`}
                      >
                        {chats.map((chat) => {
                          const active = session?.id === chat.id;
                          const title = active ? session.title : chat.title;
                          return (
                            <li key={chat.id}>
                              <button
                                className={`conversation-row ${active ? "active" : ""}`}
                                aria-current={active ? "true" : undefined}
                                title={title}
                                disabled={navigationDisabled && !active}
                                onClick={() =>
                                  void useStore
                                    .getState()
                                    .selectSession(chat.id)
                                }
                              >
                                <MessageSquare size={14} />
                                <span>{title}</span>
                                {running === chat.id && (
                                  <span className="activity-dot" />
                                )}
                              </button>
                            </li>
                          );
                        })}
                      </ul>
                    )}
                  </li>
                );
              })}
            </ul>
          ) : (
            <p className="no-conversations">
              {searchTerm
                ? "No matching projects or chats."
                : "No saved chats yet."}
            </p>
          )}
        </nav>
        <div className="sidebar-bottom">
          <div className="local-note">
            <span className="tiny-orbit" />
            <div>
              <strong>Built on your machine.</strong>
              <span>Conversations stay local.</span>
            </div>
          </div>
          <button
            className="settings-row"
            onClick={() => useStore.setState({ settings: true })}
          >
            <Settings2 size={16} />
            <span>Settings</span>
            <kbd>⌘ ,</kbd>
          </button>
          <div className="version-row">
            <span>Substrate</span>
            <span>
              v{appVersion}{" "}
              <span className={connected ? "online-dot" : "offline-dot"} />
            </span>
          </div>
        </div>
      </aside>
      <main className="main">
        <header className="topbar">
          <div className="breadcrumb">
            {!sidebar && (
              <button
                className="icon-button"
                onClick={() => setSidebar(true)}
                aria-label="Show sidebar"
              >
                <PanelLeftOpen size={17} />
              </button>
            )}
            <span>{project?.name || "Your workspace"}</span>
            <ChevronRight size={12} />
            <strong>{hasMessages ? session?.title : "New conversation"}</strong>
          </div>
          <div className="topbar-right">
            <span className="local-badge">
              <span />
              Local
            </span>
            <div className="topbar-divider" />
            <button
              className={`icon-button ${terminal ? "is-active" : ""}`}
              title="Toggle terminal"
              aria-label="Toggle terminal"
              onClick={() => setTerminal(!terminal)}
            >
              <TerminalSquare size={17} />
            </button>
            <button
              className={`icon-button ${panel ? "is-active" : ""}`}
              title="Toggle file panel"
              aria-label="Toggle file panel"
              onClick={() => setPanel(!panel)}
            >
              <PanelRight size={17} />
            </button>
          </div>
        </header>
        <section className="conversation-area">
          <div
            className="conversation-scroll"
            ref={scroll}
            onScroll={() => {
              if (scroll.current)
                stick.current =
                  scroll.current.scrollHeight -
                    scroll.current.scrollTop -
                    scroll.current.clientHeight <
                  120;
            }}
          >
            {!hasMessages ? (
              <div className="welcome">
                <div className="welcome-mark">
                  <Mark />
                  <span className="mark-orbit" />
                </div>
                <div className="eyebrow welcome-eyebrow">
                  A LITTLE CONTEXT. A LOT OF POSSIBILITY.
                </div>
                <h1>
                  What are we
                  <br />
                  building today<span>?</span>
                </h1>
                <p className="welcome-description">
                  Your ideas. Your code. A little intelligence.
                  <br />A thoughtful space to turn what’s next into what’s real.
                </p>
                <div className="starter-grid">
                  {[
                    {
                      icon: <Code2 size={19} />,
                      title: "Understand a codebase",
                      text: "Find your way around something new.",
                      prompt:
                        "Explore this repository. Explain its architecture, main entry points, and how to run it.",
                    },
                    {
                      icon: <Sparkles size={19} />,
                      title: "Make something happen",
                      text: "A new feature, from idea to commit.",
                      prompt:
                        "Help me build a new feature. First, explore the project and ask what I want to create.",
                    },
                    {
                      icon: <GitCompareArrows size={19} />,
                      title: "Give it a second look",
                      text: "Find the details worth getting right.",
                      prompt:
                        "Review the current Git changes for bugs and edge cases. Explain your findings before making edits.",
                    },
                  ].map((card) => (
                    <button
                      className="starter-card"
                      key={card.title}
                      onClick={() => {
                        setDraft(card.prompt);
                        input.current?.focus();
                      }}
                    >
                      <span className="starter-icon">{card.icon}</span>
                      <strong>{card.title}</strong>
                      <span>{card.text}</span>
                      <ArrowUp className="starter-arrow" size={15} />
                    </button>
                  ))}
                </div>
                {!project && (
                  <button
                    className="open-workspace-link"
                    onClick={() => void useStore.getState().openProject()}
                  >
                    <FolderOpen size={14} /> Start by opening a workspace{" "}
                    <ArrowRight size={13} />
                  </button>
                )}
              </div>
            ) : (
              <div className="messages">
                {session?.messages.map((message) =>
                  message.tool ? (
                    <ToolCard
                      key={message.id}
                      tool={message.tool}
                      onReview={setApproval}
                      onPreview={(preview) => {
                        setSelected(preview);
                        setFileContent(null);
                        setPanel(true);
                      }}
                    />
                  ) : (
                    <article
                      className={`message message-${message.role}`}
                      key={message.id}
                    >
                      <div className="message-avatar">
                        {message.role === "user" ? (
                          <span>Y</span>
                        ) : (
                          <Mark small />
                        )}
                      </div>
                      <div className="message-body">
                        <div className="message-author">
                          {message.role === "user" ? "You" : "Substrate"}
                          {message.role === "assistant" && (
                            <span>SUBQ SAVANT</span>
                          )}
                        </div>
                        <div className="markdown">
                          <Markdown
                            components={{
                              a: ({ href, children }) =>
                                href && /^https?:\/\//iu.test(href) ? (
                                  <a
                                    href={href}
                                    onClick={(event) => {
                                      event.preventDefault();
                                      void api
                                        .openLink(href)
                                        .catch(useStore.getState().setError);
                                    }}
                                  >
                                    {children}
                                  </a>
                                ) : (
                                  <span>{children}</span>
                                ),
                            }}
                          >
                            {message.content}
                          </Markdown>
                        </div>
                      </div>
                    </article>
                  ),
                )}
                {running && (
                  <div className="thinking">
                    <span className="activity-dot" />
                    <span>
                      {currentPending
                        ? "Waiting for your approval"
                        : "Working through it"}
                      <span className="thinking-dots">…</span>
                    </span>
                  </div>
                )}
              </div>
            )}
          </div>
          <div className="composer-wrap">
            {error && (
              <div className="error-banner" role="alert">
                <span>{error}</span>
                <button
                  className="icon-button"
                  aria-label="Dismiss error"
                  onClick={() => useStore.setState({ error: null })}
                >
                  <X size={14} />
                </button>
              </div>
            )}
            {currentPending && (
              <button
                className="pending-banner"
                onClick={() => setApproval(currentPending)}
              >
                <ShieldCheck size={15} /> Your agent has an action to review{" "}
                <span>
                  Review action <ArrowRight size={13} />
                </span>
              </button>
            )}
            <div className="composer">
              <textarea
                ref={input}
                value={draft}
                onChange={(e) => setDraft(e.target.value)}
                placeholder={
                  project
                    ? "Describe a task. Let’s work through it."
                    : "An idea, a question, a thing you want to build…"
                }
                aria-label="Message Substrate"
                rows={3}
                onKeyDown={(e) => {
                  if (
                    e.key === "Enter" &&
                    !e.shiftKey &&
                    !e.nativeEvent.isComposing
                  ) {
                    e.preventDefault();
                    submit();
                  }
                }}
              />
              <div className="composer-toolbar">
                <div className="composer-options">
                  <button
                    className="model-picker"
                    onClick={() => useStore.setState({ settings: true })}
                  >
                    <span className="model-dot" />
                    SubQ Savant
                    <ChevronDown size={12} />
                  </button>
                  <span className="composer-separator" />
                  <label className="approval-mode">
                    <ShieldCheck size={13} aria-hidden="true" />
                    <select
                      aria-label="Approval mode"
                      title="Shell commands always require approval"
                      value={approvalMode}
                      disabled={starting || changingApprovalMode}
                      onChange={(event) =>
                        void setApprovalMode(event.target.value as ApprovalMode)
                      }
                    >
                      <option value="ask">Ask before acting</option>
                      <option value="approve_edits">
                        Approve all code edits
                      </option>
                    </select>
                    <ChevronDown
                      className="approval-chevron"
                      size={12}
                      aria-hidden="true"
                    />
                  </label>
                </div>
                {running ? (
                  <button
                    className="send-button stop-button"
                    aria-label="Stop agent"
                    onClick={() =>
                      void api
                        .rpc("agent.interrupt", { session_id: running })
                        .catch(useStore.getState().setError)
                    }
                  >
                    <Square size={13} fill="currentColor" />
                  </button>
                ) : (
                  <button
                    className="send-button"
                    disabled={
                      !draft.trim() ||
                      starting ||
                      navigationDisabled ||
                      changingApprovalMode
                    }
                    aria-label="Send message"
                    onClick={submit}
                  >
                    <ArrowUp size={18} />
                  </button>
                )}
              </div>
            </div>
            <div className="composer-caption">
              <span>
                <LockIcon />{" "}
                {config?.has_key ? (
                  "Connected to Subquadratic"
                ) : (
                  <button onClick={() => useStore.setState({ settings: true })}>
                    Connect your Subquadratic account
                  </button>
                )}
              </span>
              <span>
                ↵ to send <span className="caption-dot">·</span> shift ↵ for a
                new line
              </span>
            </div>
          </div>
        </section>
        {terminal && (
          <Suspense fallback={null}>
            <Terminal />
          </Suspense>
        )}
        <footer className="statusbar">
          <span>
            <span className={connected ? "online-dot" : "offline-dot"} />
            {connected ? "Harness ready" : "Connecting to harness"}
          </span>
          <span>
            {project ? (
              <>
                <GitBranch size={12} />
                {project.branch || "Local workspace"}
              </>
            ) : (
              <>
                <Layers3 size={12} />A foundation for what’s next
              </>
            )}
          </span>
          <span className="statusbar-end">
            {running ? "Agent active" : "All systems quiet"}
            <Command size={11} />
          </span>
        </footer>
      </main>
      {panel && (
        <aside className="inspector">
          <div className="inspector-heading">
            <span>Workspace</span>
            <button
              className="icon-button"
              aria-label="Close file panel"
              onClick={() => setPanel(false)}
            >
              <X size={16} />
            </button>
          </div>
          <div className="inspector-tabs">
            <button
              className={tab === "files" ? "active" : ""}
              onClick={() => setTab("files")}
            >
              <Files size={14} />
              Files
            </button>
            <button
              className={tab === "changes" ? "active" : ""}
              onClick={() => setTab("changes")}
            >
              <GitCompareArrows size={14} />
              Changes <span>{changes.length}</span>
            </button>
            <button
              className={tab === "insights" ? "active" : ""}
              onClick={() => {
                setTab("insights");
                setSelected(null);
                setFileContent(null);
              }}
            >
              <Activity size={14} />
              Insights
            </button>
          </div>
          {session ? (
            <>
              {selected || fileContent !== null ? (
                <>
                  <div className="editor-path">
                    <File size={13} />
                    <span>{selected?.path || filePath}</span>
                    <button
                      className="icon-button"
                      title="Back to files"
                      aria-label="Back to files"
                      onClick={() => {
                        setSelected(null);
                        setFileContent(null);
                      }}
                    >
                      <X size={14} />
                    </button>
                  </div>
                  <div className="code-container">
                    <Suspense
                      fallback={
                        <div className="panel-empty">Loading editor…</div>
                      }
                    >
                      <CodeView
                        path={selected?.path || filePath}
                        content={selected?.after ?? fileContent ?? ""}
                        before={selected?.before}
                      />
                    </Suspense>
                  </div>
                </>
              ) : tab === "insights" ? (
                <Suspense
                  fallback={
                    <div className="panel-empty">Loading insights…</div>
                  }
                >
                  <Insights
                    onPreview={(preview) => {
                      setSelected(preview);
                      setFileContent(null);
                    }}
                  />
                </Suspense>
              ) : (
                <div className="file-list">
                  {tab === "files" ? (
                    <>
                      <div className="tree-root">
                        <ChevronDown size={12} />
                        <FolderOpen size={14} />
                        {project?.name}
                      </div>
                      <FileTree onOpen={(path) => void openFile(path)} />
                    </>
                  ) : gitError ? (
                    <div className="panel-empty">
                      <GitBranch size={24} />
                      <p>Git is unavailable for this workspace.</p>
                      <small>{gitError}</small>
                    </div>
                  ) : changes.length ? (
                    changes.map((change) => (
                      <button
                        className="change-row"
                        key={change.path}
                        onClick={() => void openFile(change.path, true)}
                      >
                        <FileCode2 size={14} />
                        <span>{change.path}</span>
                        <code>{change.status}</code>
                      </button>
                    ))
                  ) : (
                    <div className="panel-empty">
                      <Check size={25} />
                      <p>A clean slate.</p>
                      <small>No uncommitted changes.</small>
                    </div>
                  )}
                </div>
              )}
            </>
          ) : (
            <div className="panel-empty">
              <FolderOpen size={28} />
              <p>A place for your code.</p>
              <small>
                Open a workspace to explore files
                <br />
                and review your agent’s changes.
              </small>
              <Button
                variant="outline"
                size="sm"
                onClick={() => void useStore.getState().openProject()}
              >
                Open workspace
              </Button>
            </div>
          )}
          <div className="inspector-footer">
            <ShieldCheck size={13} />
            Every change, in your hands.
          </div>
        </aside>
      )}
      <Settings />
      <Dialog.Root
        open={!!activeApproval && activeApproval.status === "approval"}
        onOpenChange={(open) => {
          if (!open) setApproval(null);
        }}
      >
        <Dialog.Portal>
          <Dialog.Overlay className="modal-overlay" />
          <Dialog.Content
            className={`approval-modal ${activeApproval?.preview ? "approval-diff" : ""}`}
          >
            <div className="modal-top">
              <span className="eyebrow">
                <ShieldCheck size={14} /> YOUR APPROVAL IS NEEDED
              </span>
              <Dialog.Close className="icon-button" aria-label="Close review">
                <X size={18} />
              </Dialog.Close>
            </div>
            <Dialog.Title>
              {activeApproval?.name === "apply_patch"
                ? "A change worth a look."
                : "Ready to run this?"}
            </Dialog.Title>
            <Dialog.Description>
              {activeApproval?.preview
                ? activeApproval.preview.path
                : "This command runs on your machine with your user permissions, outside an OS sandbox."}
            </Dialog.Description>
            {activeApproval?.name === "run_check" && (
              <p>{String(activeApproval.arguments.purpose)}</p>
            )}
            {activeApproval?.preview ? (
              <div className="approval-editor">
                <Suspense
                  fallback={<div className="panel-empty">Loading diff…</div>}
                >
                  <CodeView
                    path={activeApproval.preview.path}
                    content={activeApproval.preview.after}
                    before={activeApproval.preview.before}
                  />
                </Suspense>
              </div>
            ) : (
              <pre className="command-preview">
                <span>$ </span>
                {String(activeApproval?.arguments.command || "")}
              </pre>
            )}
            <div className="modal-actions">
              <Button
                variant="outline"
                disabled={approving}
                onClick={() => void decide(false)}
              >
                Decline
              </Button>
              <Button disabled={approving} onClick={() => void decide(true)}>
                {approving ? (
                  <Loader2 size={14} className="spin" />
                ) : (
                  <Check size={14} />
                )}{" "}
                {activeApproval?.preview ? "Approve edit" : "Approve command"}
              </Button>
            </div>
          </Dialog.Content>
        </Dialog.Portal>
      </Dialog.Root>
    </div>
  );
}
function LockIcon() {
  return <CircleDot size={11} />;
}
