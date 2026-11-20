"""Compare context selectors through the Rust harness on isolated lookup and editing tasks."""
import argparse
from concurrent.futures import ThreadPoolExecutor, as_completed
from contextlib import contextmanager
from dataclasses import dataclass
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import json
import os
from pathlib import Path
import queue
import shlex
import shutil
import subprocess
import sys
import tempfile
import threading
import time

from integration import Client, ROOT

@dataclass(frozen=True)
class Case:
    name: str
    task: str
    files: tuple
    answer: str = ""
    # Each edit is (path, original text, replacement text).
    edits: tuple = ()

    @property
    def check(self):
        if self.name == "edit-retry":
            code = "from billing.recovery import RETRY_WINDOW_MINUTES; assert RETRY_WINDOW_MINUTES == 60"
        else:
            code = "import runpy; runpy.run_path('tests/test_download.py')['test_csv_download_limit']()"
        return "python3 -B -c " + shlex.quote(code)

    @property
    def prompt(self):
        if self.edits:
            return self.task + " Edit the source using file tools, then verify with this exact command: " + self.check
        return self.task + " Reply with only the integer. Use read-only tools if needed."


RECOVERY = "billing/recovery.py"
EXPORT = "reports/batch.py"
SESSION = "auth/lifecycle.py"
EXPORT_TEST = "tests/test_download.py"

CASES = (
    Case("grace-period", "How many minutes can a failed payment be retried for?", (RECOVERY,), "47"),
    Case("export-limit", "How many rows can the CSV downloader include?", (EXPORT,), "275"),
    Case("session-idle", "How many seconds can an inactive session remain open?", (SESSION,), "913"),
    Case("edit-retry", "Change the failed payment retry window to 60 minutes.", (RECOVERY,),
         edits=((RECOVERY, "RETRY_WINDOW_MINUTES = 47", "RETRY_WINDOW_MINUTES = 60"),)),
    Case("edit-export", "Raise the CSV download limit to 500 rows and update the test expectation.",
         (EXPORT, EXPORT_TEST),
         edits=((EXPORT, "MAX_DOWNLOAD_RECORDS = 275", "MAX_DOWNLOAD_RECORDS = 500"),
                (EXPORT_TEST, "assert MAX_DOWNLOAD_RECORDS == 275", "assert MAX_DOWNLOAD_RECORDS == 500"))),
)
REPO_SIZES = ("small", "large")
BUDGET = 32 * 1024
TERMINAL = {"agent.completed", "agent.failed", "agent.interrupted"}


def tree_contents(repo):
    return {file.relative_to(repo).as_posix(): file.read_bytes()
            for file in repo.rglob("*") if file.is_file()}


def make_repo(repo, size):
    # Each main source file fits, but any pair would exceed the context budget.
    padding = "# Fixture padding\n" * 1100
    files = {
        RECOVERY: "RETRY_WINDOW_MINUTES = 47\n" + padding,
        EXPORT: "MAX_DOWNLOAD_RECORDS = 275\n" + padding,
        SESSION: "IDLE_TIMEOUT_SECONDS = 913\n" + padding,
        EXPORT_TEST: "from reports.batch import MAX_DOWNLOAD_RECORDS\n\ndef test_csv_download_limit():\n"
                     "    assert MAX_DOWNLOAD_RECORDS == 275\n",
        ".gitignore": "ignored.py\n",
        "ignored.py": "# Ignored fixture file\n",
        "a_config.py": "DEFAULT_PORT = 8080\n" + padding,
    }
    if size == "large":
        # Mix unrelated modules with nearby concepts that compete for lexical rank.
        for domain, concepts in {
            "payments": ("payment retry audit", "failed payment recovery queue"),
            "exports": ("CSV downloader telemetry", "download limit notification"),
            "accounts": ("inactive session audit", "session lifecycle event"),
            "catalog": ("product inventory indexing", "warehouse reconciliation"),
        }.items():
            for i in range(128):
                concept = concepts[i % len(concepts)]
                files[f"services/{domain}/worker_{i:03}.py"] = (
                    f'"""Background worker for {concept}. No runtime configuration here."""\n\n'
                    f'WORKER_ID = "{domain}-{i}"\n\n'
                    'def label(event):\n    return {"worker": WORKER_ID, "event": event}\n'
                )
    for path, source in files.items():
        file = repo / path
        file.parent.mkdir(parents=True, exist_ok=True)
        file.write_text(source, encoding="utf-8")


class Fixture(BaseHTTPRequestHandler):
    """Scripted model behavior for checking the runner and feedback loop offline."""

    def log_message(self, *args):
        pass

    def do_POST(self):
        request = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
        if self.path == "/v1/systemone":
            question = request["state"]["task"]
            case = next(case for case in CASES if case.task in question)
            # Exercise recovery from a confident Snap miss as well as a hit.
            picks = ("a_config.py",) if case.name == "session-idle" else case.files
            answers = {
                file["id"]: {"type": "score", "score": 3 if file["path"] in picks else 0,
                             "confidence": 0.95}
                for file in request["state"]["files"]
            }
            self.send_response(200)
            self.send_header("Content-Type", "application/json")
            self.end_headers()
            self.wfile.write(json.dumps({"answers": answers, "usage": {
                "input_tokens": 100, "output_tokens": 0,
            }}).encode())
            return

        assert self.path == "/v1/chat/completions"
        messages = request["messages"]
        task = next(m["content"] for m in reversed(messages) if m["role"] == "user")
        case = next(case for case in CASES if case.prompt == task)
        rows = (json.loads(line) for line in messages[1]["content"].splitlines()
                if line.startswith("{"))
        context = {row["path"] for row in rows if "path" in row}
        assert "ignored.py" not in context
        missing = [path for path in case.files if path not in context]
        tool_names = {call["function"]["name"] for message in messages
                      for call in message.get("tool_calls", [])}
        tools = []
        if "run_check" in tool_names:
            assert json.loads(messages[-1]["content"])["exit_code"] == 0
            delta = {"content": "Edited."}
        elif "apply_patch" in tool_names:
            tools = [("run_check", {"command": case.check,
                                    "purpose": "Assert the updated limit"})]
        elif missing and "read_file" in tool_names:
            delta = {"content": "promotion failed"}
        elif missing:
            if case.name == "session-idle":
                missing += ["ignored.py", "missing.py"]
            tools = [("read_file", {"path": "./" + path}) for path in missing]
        elif case.edits:
            tools = [("apply_patch", {"path": path, "old_text": old, "new_text": new})
                     for path, old, new in case.edits]
        else:
            delta = {"content": case.answer}
        finish = "stop"
        if tools:
            delta = {"tool_calls": [
                {"index": index, "id": f"{name}-{index}", "function": {
                    "name": name, "arguments": json.dumps(args),
                }} for index, (name, args) in enumerate(tools)
            ]}
            finish = "tool_calls"
        self.send_response(200)
        self.send_header("Content-Type", "text/event-stream")
        self.end_headers()
        for choice in [{"delta": delta}, {"delta": {}, "finish_reason": finish}]:
            self.wfile.write(("data: " + json.dumps({"choices": [choice]}) + "\n\n").encode())
        self.wfile.write(b"data: [DONE]\n\n")


@contextmanager
def provider(live):
    if live:
        yield "https://api.subq.ai/v1", os.environ.get("SUBQ_API_KEY", "")
        return
    with ThreadingHTTPServer(("127.0.0.1", 0), Fixture) as server:
        thread = threading.Thread(target=server.serve_forever, daemon=True)
        thread.start()
        try:
            yield f"http://127.0.0.1:{server.server_port}/v1", "fixture-key"
        finally:
            server.shutdown()
            thread.join()


def run_case(directory, case, size, mode, url, key, timeout, **generation):
    repo = directory / "repo"
    make_repo(repo, size)
    initial = tree_contents(repo)
    expected = initial.copy()
    for path, old, new in case.edits:
        assert expected[path].count(old.encode()) == 1
        expected[path] = expected[path].replace(old.encode(), new.encode())
    targets = {edit[0] for edit in case.edits}
    client = Client(directory, url, key=key)
    try:
        client.rpc("config.update", model="subq-savant-1.4", context_budget_bytes=BUDGET, max_steps=8)
        session = client.rpc("session.create", project=str(repo))["id"]
        client.rpc("agent.start", session_id=session, task=case.prompt,
                   context_selection=mode, approval_mode="ask",
                   required_checks=[case.check] if case.edits else [], **generation)
        rejected_tools = 0
        approved_edits = 0
        try:
            # A single deadline covers all events, including rejected approvals.
            deadline = time.monotonic() + timeout
            while True:
                event = client.wait_event(TERMINAL | {"tool.approval.required"},
                                          timeout=max(0, deadline - time.monotonic()))
                if event["method"] in TERMINAL:
                    break
                tool = event["params"]["tool"]
                path = tool.get("preview", {}).get("path") if tool.get("preview") else None
                is_edit = tool["name"] == "apply_patch" and path is not None and Path(path).as_posix() in targets
                is_check = bool(case.edits) and tool["name"] == "run_check" and tool["arguments"]["command"] == case.check
                approved = is_edit or is_check
                approved_edits += int(is_edit)
                rejected_tools += int(not approved)
                client.rpc("tool.approve" if approved else "tool.reject",
                           session_id=session, tool_id=tool["id"])
        except (TimeoutError, queue.Empty):
            client.rpc("agent.interrupt", session_id=session)
            client.wait_event(TERMINAL)
            event = {"method": "timeout", "params": {}}

        usage = client.rpc("usage.get", session_id=session)
        run = usage["runs"][0]
        generated = sorted((c for c in usage["calls"] if c["kind"] == "generation"),
                           key=lambda c: (c["started_at"], c["context_revision"]))
        first = client.rpc("context.get", session_id=session,
                           snapshot_id=generated[0]["snapshot_id"])["snapshot"] if generated else None
        latest = client.rpc("context.get", session_id=session)["snapshot"]
        selection = [c for c in usage["calls"] if c["kind"] == "context_selection"]
        messages = client.rpc("session.resume", session_id=session)["messages"]
        answer = next((m["content"].strip() for m in reversed(messages) if m["role"] == "assistant"), "")
        reads = run["context_reads"]
        actual = tree_contents(repo)
        output_correct = actual == expected and (bool(case.edits) or answer == case.answer)
        included = {file["path"] for file in first["files"]} if first else set()
        check_passed = False
        for message in messages:
            tool = message.get("tool", {})
            if tool.get("status") != "completed":
                continue
            if tool.get("name") == "apply_patch":
                check_passed = False
            elif case.edits and tool.get("name") == "run_check" and tool["arguments"]["command"] == case.check:
                check_passed = True
        calls = generated + selection
        token_values = [(call["metrics"].get("usage") or {}).get("input_tokens") for call in calls]
        return {
            "case": case.name, "task_type": "edit" if case.edits else "lookup",
            "repo_size": size, "repo_files": len(initial),
            "mode": mode, "status": event["method"],
            "approved_edits": approved_edits, "rejected_tools": rejected_tools,
            "changed_files": sorted(path for path in initial.keys() | actual.keys()
                                    if initial.get(path) != actual.get(path)),
            "error": event["params"].get("message"),
            "success": event["method"] == "agent.completed" and output_correct and not rejected_tools
                       and (not case.edits or check_passed),
            "task_success": event["method"] == "agent.completed" and output_correct
                            and (not case.edits or check_passed),
            "output_correct": output_correct,
            "exact_check_passed": check_passed if case.edits else None,
            "answer": answer if not case.edits else None,
            "all_provider_input_tokens": sum(token_values) if calls and None not in token_values else None,
            "calls": [
                {field: call[field] for field in ("kind", "model", "status", "tool_calls", "metrics")}
                for call in calls
            ],
            "first_snapshot_required_ranks": [
                rank for rank in first["selection"]["ranks"] if rank["path"] in case.files
            ] if first else [],
            "tool_results": [
                {field: message["tool"].get(field) for field in ("name", "status")}
                for message in messages if message.get("tool")
            ],
            "first_snapshot_strategy": first["selection"]["strategy"] if first else None,
            "first_snapshot_hit": set(case.files) <= included,
            "first_snapshot_required_files": sorted(set(case.files) & included),
            "required_files": list(case.files),
            "eligible_files": first["selection"]["candidate_files"] if first else None,
            "first_snapshot_bytes": generated[0]["context_bytes"] if generated else None,
            "generation_context_bytes": sum(c["context_bytes"] for c in generated),
            "generation_calls": len(generated),
            "snap_calls": len(selection),
            "snap_ms": sum(c["metrics"]["duration_ms"] or 0 for c in selection),
            "elapsed_ms": run["ended_at"] - run["started_at"] if run["ended_at"] is not None else None,
            "successful_reads": reads["successful"],
            "missed_reads": reads["missed"],
            "missed_paths": reads["missed_paths"],
            "promoted_paths": latest["selection"].get("promoted_paths", []),
        }
    finally:
        client.close()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--live", action="store_true", help="Use SubQ API credits and send the synthetic tasks to SubQ")
    parser.add_argument("--generator", choices=("subq", "codex"), default="subq")
    parser.add_argument("--model", help="Codex model ID when --generator=codex")
    parser.add_argument("--modes", nargs="+", choices=("local", "snap", "auto"))
    parser.add_argument("--jobs", type=int, default=1, help="Number of isolated harness processes to run in parallel")
    parser.add_argument("--repeats", type=int, default=1)
    parser.add_argument("--timeout", type=float, default=120, help="Seconds per run")
    args = parser.parse_args()
    if args.repeats < 1 or args.timeout <= 0 or not 1 <= args.jobs <= 8:
        parser.error("repeats and timeout must be positive and jobs must be 1–8")
    codex = args.generator == "codex"
    modes = tuple(dict.fromkeys(args.modes or (("local",) if codex else ("local", "snap"))))
    live = args.live or codex
    if live and (not codex or any(mode != "local" for mode in modes)) and not os.environ.get("SUBQ_API_KEY", "").strip():
        parser.error("live SubQ generation or Snap selection requires SUBQ_API_KEY")
    generation = {}
    if codex:
        executable = os.environ.get("SUBSTRATE_EVAL_CODEX") or shutil.which("codex")
        if not executable or not args.model:
            parser.error("Codex evaluation requires the Codex CLI and --model")
        os.environ["SUBSTRATE_EVAL_CODEX"] = str(Path(executable).absolute())
        generation["eval_codex_model"] = args.model
    elif args.model:
        parser.error("--model requires --generator=codex")
    subprocess.run(["cargo", "build", "-q", "-p", "agent-server"], cwd=ROOT, check=True)
    results = []
    with tempfile.TemporaryDirectory(prefix="substrate-context-eval-") as tmp, provider(live) as (url, key), \
            ThreadPoolExecutor(max_workers=args.jobs) as workers:
        pending = {}
        for trial in range(1, args.repeats + 1):
            for size_index, size in enumerate(REPO_SIZES):
                for index, case in enumerate(CASES):
                    order = modes if (trial - 1 + index + size_index) % 2 == 0 else modes[::-1]
                    for mode in order:
                        name = f"{trial}-{size}-{case.name}-{mode}"
                        future = workers.submit(run_case, Path(tmp) / name, case, size, mode, url,
                                                key, args.timeout, **generation)
                        pending[future] = trial, size, case.name, mode
        for future in as_completed(pending):
            trial, size, case_name, mode = pending[future]
            result = future.result()
            results.append({"trial": trial, **result})
            print(f'{len(results)}/{len(pending)} {size} {case_name} {mode} {result["status"]} '
                  f'success={result["success"]}', file=sys.stderr, flush=True)
    results.sort(key=lambda row: (row["trial"], row["repo_size"], row["case"], row["mode"]))
    summary = {}
    for mode in modes:
        rows = [r for r in results if r["mode"] == mode]
        summary[mode] = {"runs": len(rows)}
        for key in ("success", "first_snapshot_hit", "missed_reads", "generation_context_bytes",
                    "generation_calls", "snap_calls", "snap_ms", "elapsed_ms", "all_provider_input_tokens"):
            values = [row[key] for row in rows]
            summary[mode][key] = sum(values) if all(value is not None for value in values) else None
    print(json.dumps({"provider": args.generator if live else "scripted fixture",
                      "model": args.model if codex else "subq-savant-1.4",
                      "budget_bytes": BUDGET, "totals": summary, "runs": results}, indent=2))
    if not live:
        # A fixture checks the measurement pipeline. Its results say nothing about real model quality.
        assert all(r["success"] for r in results)
        for row in results:
            if row["mode"] in ("local", "snap"):
                assert (row["snap_calls"] > 0) == (row["mode"] == "snap")
            assert row["successful_reads"] == row["missed_reads"]
            if row["repo_size"] == "large":
                assert row["eligible_files"] > 128
            if row["case"] == "export-limit":
                assert row["first_snapshot_hit"], "The CSV implementation should survive the distractors"
                if row["mode"] == "snap":
                    assert row["first_snapshot_required_ranks"][0]["snap_candidate"]
            if row["case"] == "session-idle" and not row["first_snapshot_hit"]:
                assert row["missed_reads"] == 2
                ignored = next(m for m in row["missed_paths"] if m["path"] == "ignored.py")
                assert not ignored["eligible"] and "ignored.py" not in row["promoted_paths"]
            if row["task_type"] == "edit":
                assert row["changed_files"] == sorted(row["required_files"])
                assert row["approved_edits"] == len(row["required_files"])


if __name__ == "__main__":
    main()
