import { useEffect, useRef } from "react";
import { Terminal as XTerm } from "@xterm/xterm";
import { FitAddon } from "@xterm/addon-fit";
import { TerminalSquare } from "lucide-react";
import { useStore } from "../lib/store";
import "@xterm/xterm/css/xterm.css";
export default function Terminal() {
  const host = useRef<HTMLDivElement>(null);
  const terminal = useRef<XTerm | null>(null);
  const written = useRef("");
  const output = useStore((s) => s.terminalOutput);
  useEffect(() => {
    const term = new XTerm({
      convertEol: true,
      disableStdin: true,
      fontSize: 12,
      fontFamily: "Menlo, monospace",
      cursorBlink: false,
      theme: {
        background: "#242720",
        foreground: "#d6d9c9",
        cursor: "#c5d293",
      },
    });
    const fit = new FitAddon();
    term.loadAddon(fit);
    term.open(host.current!);
    terminal.current = term;
    term.writeln(
      "\x1b[90mCommand output appears here after you approve a command.\x1b[0m\r\n",
    );
    const observer = new ResizeObserver(() => fit.fit());
    observer.observe(host.current!);
    fit.fit();
    return () => {
      observer.disconnect();
      term.dispose();
      terminal.current = null;
      written.current = "";
    };
  }, []);
  useEffect(() => {
    if (!terminal.current) return;
    if (!output.startsWith(written.current)) terminal.current.clear();
    terminal.current.write(
      output.startsWith(written.current)
        ? output.slice(written.current.length)
        : output,
    );
    written.current = output;
  }, [output]);
  return (
    <div className="terminal-panel">
      <div className="terminal-title">
        <TerminalSquare size={14} /> Agent terminal{" "}
        <span>Read only · 120s timeout</span>
      </div>
      <div className="terminal-host" ref={host} />
    </div>
  );
}
