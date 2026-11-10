import {
  app,
  BrowserWindow,
  dialog,
  ipcMain,
  Menu,
  safeStorage,
} from "electron";
import { spawn, type ChildProcessWithoutNullStreams } from "node:child_process";
import { join, dirname } from "node:path";
import { fileURLToPath } from "node:url";
import { promises as fs } from "node:fs";
import type { AgentEvent } from "../shared/types";

const here = dirname(fileURLToPath(import.meta.url));
let window: BrowserWindow | null = null;
let agent: ChildProcessWithoutNullStreams | null = null;
let ready: Promise<void>;
let sequence = 0;
const pending = new Map<
  number,
  {
    resolve: (v: unknown) => void;
    reject: (e: Error) => void;
    timer: NodeJS.Timeout;
  }
>();
const allowed = new Set([
  "ping",
  "config.get",
  "config.update",
  "models.list",
  "session.create",
  "session.list",
  "session.resume",
  "agent.start",
  "agent.interrupt",
  "session.cancel",
  "tool.approve",
  "tool.reject",
  "files.list",
  "files.read",
  "git.status",
  "git.branch",
  "git.file",
  "context.get",
  "context.refresh",
  "context.history",
  "context.file",
  "usage.get",
]);
function rpc(
  method: string,
  params: Record<string, unknown> = {},
): Promise<unknown> {
  return new Promise((resolve, reject) => {
    if (!agent || agent.killed) {
      reject(new Error("Agent is offline. Restart Substrate."));
      return;
    }
    const id = ++sequence;
    const timer = setTimeout(() => {
      pending.delete(id);
      reject(new Error("Agent request timed out"));
    }, 30_000);
    pending.set(id, { resolve, reject, timer });
    agent.stdin.write(
      JSON.stringify({ jsonrpc: "2.0", id, method, params }) + "\n",
      (error) => {
        if (error) {
          clearTimeout(timer);
          pending.delete(id);
          reject(new Error("Agent connection closed"));
        }
      },
    );
  });
}
function offline(message: string) {
  for (const p of pending.values()) {
    clearTimeout(p.timer);
    p.reject(new Error(message));
  }
  pending.clear();
  window?.webContents.send("agent:event", {
    method: "connection.closed",
    params: { message },
  } satisfies AgentEvent);
}
async function startAgent() {
  const binary = app.isPackaged
    ? join(process.resourcesPath, "agent-server")
    : join(app.getAppPath(), "target/debug/agent-server");
  agent = spawn(binary, [], {
    env: {
      ...process.env,
      SUBSTRATE_RG: app.isPackaged
        ? join(process.resourcesPath, "bin/rg")
        : process.env.SUBSTRATE_RG,
      SUBSTRATE_DB:
        process.env.SUBSTRATE_DB ??
        join(app.getPath("userData"), "sessions.db"),
    },
    stdio: ["pipe", "pipe", "pipe"],
  });
  let buffer = "";
  agent.stdout.setEncoding("utf8");
  agent.stdout.on("data", (chunk: string) => {
    buffer += chunk;
    if (buffer.length > 8 * 1024 * 1024) {
      offline("Invalid frame from agent");
      agent?.kill();
      return;
    }
    let newline: number;
    while ((newline = buffer.indexOf("\n")) >= 0) {
      const line = buffer.slice(0, newline);
      buffer = buffer.slice(newline + 1);
      try {
        const message = JSON.parse(line);
        if (message.id !== undefined) {
          const p = pending.get(message.id);
          if (p) {
            clearTimeout(p.timer);
            pending.delete(message.id);
            if (message.error) p.reject(new Error(message.error.message));
            else p.resolve(message.result);
          }
        } else if (typeof message.method === "string")
          window?.webContents.send("agent:event", message);
      } catch {
        offline("Invalid JSON from agent");
      }
    }
  });
  agent.stderr.on("data", (chunk: Buffer) =>
    console.error("[harness]", chunk.toString()),
  );
  agent.on("error", () =>
    offline("Unable to start the Rust harness. Run npm run dev to build it."),
  );
  agent.on("exit", () => {
    agent = null;
    offline("Rust harness stopped. Restart Substrate to reconnect.");
  });
  await rpc("ping");
  try {
    const encrypted = await fs.readFile(
      join(app.getPath("userData"), "subq-key.enc"),
    );
    if (await safeStorage.isAsyncEncryptionAvailable()) {
      const key = await safeStorage.decryptStringAsync(encrypted);
      await rpc("config.credentials", { key });
    }
  } catch (error) {
    if ((error as NodeJS.ErrnoException).code !== "ENOENT")
      console.error("Could not restore the saved credential");
  }
}
function validateSender(event: Electron.IpcMainInvokeEvent) {
  if (
    !window ||
    event.sender !== window.webContents ||
    event.senderFrame !== window.webContents.mainFrame
  )
    throw new Error("Unauthorized IPC sender");
}
ipcMain.handle("agent:rpc", async (event, method: unknown, params: unknown) => {
  validateSender(event);
  if (typeof method !== "string" || !allowed.has(method))
    throw new Error("Unsupported operation");
  if (
    params !== undefined &&
    (!params || typeof params !== "object" || Array.isArray(params))
  )
    throw new Error("Invalid parameters");
  if (JSON.stringify(params ?? {}).length > 2 * 1024 * 1024)
    throw new Error("Request too large");
  await ready;
  return rpc(method, (params ?? {}) as Record<string, unknown>);
});
ipcMain.handle("project:open", async (event) => {
  validateSender(event);
  await ready;
  const result = await dialog.showOpenDialog(window!, {
    title: "Open a workspace",
    buttonLabel: "Open workspace",
    properties: ["openDirectory"],
  });
  if (result.canceled || !result.filePaths[0]) return null;
  return rpc("project.open", { path: result.filePaths[0] });
});
ipcMain.handle("credentials:set", async (event, key: unknown) => {
  validateSender(event);
  await ready;
  if (typeof key !== "string" || key.length > 8192)
    throw new Error("Invalid API key");
  if (key) {
    if (
      !(await safeStorage.isAsyncEncryptionAvailable()) ||
      (process.platform === "linux" &&
        safeStorage.getSelectedStorageBackend() === "basic_text")
    )
      throw new Error("OS credential encryption is unavailable");
    const encrypted = await safeStorage.encryptStringAsync(key.trim());
    await fs.mkdir(app.getPath("userData"), { recursive: true });
    await fs.writeFile(
      join(app.getPath("userData"), "subq-key.enc"),
      encrypted,
      { mode: 0o600 },
    );
  } else {
    await fs.rm(join(app.getPath("userData"), "subq-key.enc"), { force: true });
  }
  await rpc("config.credentials", { key: key.trim() });
});
function createWindow() {
  window = new BrowserWindow({
    width: 1440,
    height: 940,
    minWidth: 1000,
    minHeight: 700,
    title: "Substrate",
    backgroundColor: "#f6f5f1",
    titleBarStyle: "hiddenInset",
    trafficLightPosition: { x: 19, y: 20 },
    webPreferences: {
      preload: join(here, "../preload/index.js"),
      contextIsolation: true,
      nodeIntegration: false,
      sandbox: true,
      webSecurity: true,
    },
  });
  window.webContents.setWindowOpenHandler(() => ({ action: "deny" }));
  window.webContents.on("will-navigate", (event) => event.preventDefault());
  window.webContents.session.setPermissionRequestHandler(
    (_contents, _permission, callback) => callback(false),
  );
  if (!app.isPackaged && process.env.ELECTRON_RENDERER_URL)
    void window.loadURL(process.env.ELECTRON_RENDERER_URL);
  else void window.loadFile(join(here, "../renderer/index.html"));
  window.on("closed", () => {
    window = null;
  });
}
app.setName("Substrate");
app.whenReady().then(() => {
  Menu.setApplicationMenu(
    Menu.buildFromTemplate([
      {
        label: "Substrate",
        submenu: [
          { role: "about" },
          { type: "separator" },
          { role: "hide" },
          { role: "quit" },
        ],
      },
      {
        label: "Edit",
        submenu: [
          { role: "undo" },
          { role: "redo" },
          { type: "separator" },
          { role: "cut" },
          { role: "copy" },
          { role: "paste" },
          { role: "selectAll" },
        ],
      },
      {
        label: "View",
        submenu: [
          { role: "reload" },
          { role: "toggleDevTools" },
          { role: "resetZoom" },
          { role: "zoomIn" },
          { role: "zoomOut" },
        ],
      },
      {
        label: "Window",
        submenu: [{ role: "minimize" }, { role: "zoom" }, { role: "front" }],
      },
    ]),
  );
  ready = startAgent();
  ready.catch((error) => console.error(error.message));
  createWindow();
  app.on("activate", () => {
    if (!window) createWindow();
  });
});
app.on("window-all-closed", () => {
  if (process.platform !== "darwin") app.quit();
});
app.on("before-quit", () => {
  agent?.stdin.end();
  const child = agent;
  setTimeout(() => child?.kill("SIGTERM"), 1500).unref();
});
