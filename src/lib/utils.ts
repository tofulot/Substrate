export function workspaceName(path: string) {
  return path.split(/[\\/]/).filter(Boolean).pop() || "Workspace";
}
