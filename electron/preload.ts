import { contextBridge, ipcRenderer } from "electron";
import type { AgentEvent, DesktopAPI } from "../shared/types";
const api: DesktopAPI = {
  rpc: (method, params) => ipcRenderer.invoke("agent:rpc", method, params),
  openProject: () => ipcRenderer.invoke("project:open"),
  createProject: (name) => ipcRenderer.invoke("project:create", name),
  setApiKey: (key) => ipcRenderer.invoke("credentials:set", "subq", key),
  setSearchApiKey: (key) =>
    ipcRenderer.invoke("credentials:set", "search", key),
  openLink: (url) => ipcRenderer.invoke("link:open", url),
  onEvent: (callback) => {
    const handler = (_event: Electron.IpcRendererEvent, message: AgentEvent) =>
      callback(message);
    ipcRenderer.on("agent:event", handler);
    return () => ipcRenderer.removeListener("agent:event", handler);
  },
  platform: process.platform,
};
contextBridge.exposeInMainWorld("substrate", api);
