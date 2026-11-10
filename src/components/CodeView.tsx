import { useLayoutEffect, useRef } from "react";
import "../lib/editor-icons.css";
import * as monaco from "monaco-editor/editor/editor.api";
import EditorWorker from "monaco-editor/editor/editor.worker?worker";
import "monaco-editor/languages/definitions/typescript/register";
import "monaco-editor/languages/definitions/javascript/register";
import "monaco-editor/languages/definitions/rust/register";
import "monaco-editor/languages/definitions/python/register";
import "monaco-editor/languages/definitions/markdown/register";
import "monaco-editor/languages/definitions/css/register";
import "monaco-editor/languages/definitions/html/register";
import "monaco-editor/languages/definitions/yaml/register";
import "monaco-editor/languages/definitions/ini/register";
self.MonacoEnvironment = { getWorker: () => new EditorWorker() };
monaco.editor.defineTheme("substrate", {
  base: "vs",
  inherit: true,
  rules: [{ token: "comment", foreground: "999A8E" }],
  colors: {
    "editor.background": "#FAFAF7",
    "editor.lineHighlightBackground": "#F2F2EB",
    "editorLineNumber.foreground": "#B9BAAD",
    "editor.selectionBackground": "#DDE5BD",
    "diffEditor.insertedTextBackground": "#B3C68430",
    "diffEditor.removedTextBackground": "#D58B7B25",
  },
});
const options: monaco.editor.IStandaloneEditorConstructionOptions = {
  readOnly: true,
  minimap: { enabled: false },
  fontSize: 12,
  fontFamily: '"SFMono-Regular", Consolas, monospace',
  lineNumbersMinChars: 3,
  scrollBeyondLastLine: false,
  padding: { top: 20 },
  wordWrap: "on",
  renderLineHighlight: "none",
  automaticLayout: true,
};
function language(path: string) {
  return (
    (
      {
        ts: "typescript",
        tsx: "typescript",
        js: "javascript",
        jsx: "javascript",
        rs: "rust",
        json: "json",
        md: "markdown",
        py: "python",
        css: "css",
        html: "html",
        toml: "ini",
        yml: "yaml",
      } as Record<string, string>
    )[path.split(".").pop()!] || "plaintext"
  );
}
export default function CodeView({
  path,
  content,
  before,
}: {
  path: string;
  content: string;
  before?: string;
}) {
  const host = useRef<HTMLDivElement>(null);
  useLayoutEffect(() => {
    if (!host.current) return;
    const lang = language(path);
    const modified = monaco.editor.createModel(content, lang);
    const original =
      before !== undefined ? monaco.editor.createModel(before, lang) : null;
    if (original) {
      const editor = monaco.editor.createDiffEditor(host.current, {
        ...options,
        theme: "substrate",
        renderSideBySide: false,
        originalEditable: false,
      });
      editor.setModel({ original, modified });
      return () => {
        // Detach both models before disposal. Disposing an attached model can race
        // Monaco's asynchronous diff computation when a review closes.
        editor.setModel(null);
        editor.dispose();
        original.dispose();
        modified.dispose();
      };
    }
    const editor = monaco.editor.create(host.current, {
      ...options,
      theme: "substrate",
      model: modified,
    });
    return () => {
      editor.setModel(null);
      editor.dispose();
      modified.dispose();
    };
  }, [path, content, before]);
  return (
    <div
      ref={host}
      style={{ height: "100%", width: "100%" }}
      aria-label={`${before !== undefined ? "Diff" : "File"}: ${path}`}
    />
  );
}
