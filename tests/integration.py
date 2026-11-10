"""Exercise the real Rust server against a deterministic SubQ-compatible HTTP fixture.
No paid API calls or user credentials. Verifies orchestration, approvals, history,
protocol correlation, cancellation and workspace validation across process restarts.
"""
import json
import os
from pathlib import Path
import queue
import subprocess
import sqlite3
import tempfile
import threading
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

ROOT = Path(__file__).resolve().parent.parent
requests = []
stale_ready = threading.Event()

def call(name, args, index=0):
    encoded = json.dumps(args)
    return [
        {"choices": [{"delta": {"tool_calls": [{"index": index, "id": f"call-{len(requests)}-{index}", "type": "function", "function": {"name": name, "arguments": encoded[:3]}}]}, "finish_reason": None}]},
        {"choices": [{"delta": {"tool_calls": [{"index": index, "function": {"arguments": encoded[3:]}}]}, "finish_reason": None}]},
    ]

class Model(BaseHTTPRequestHandler):
    def log_message(self, *args):
        pass

    def do_POST(self):
        body = json.loads(self.rfile.read(int(self.headers['Content-Length'])))
        requests.append(body)
        assert self.path == '/v1/chat/completions'
        assert self.headers['Authorization'] == 'Bearer fixture-key'
        assert body['model'] == 'subq-preview' and body['stream'] and body['store'] is False
        assert body['tools'][0]['function']['parameters']['additionalProperties'] is False
        messages = body['messages']
        task = next(m['content'] for m in reversed(messages) if m['role'] == 'user')
        outputs = [m for m in messages if m['role'] == 'tool']
        assert messages[1]['role'] == 'user' and messages[1]['content'].startswith('Repository snapshot ')
        assert 'PRIVATE_TEST_SECRET' not in messages[1]['content']
        assert 'ignored-source-data' not in messages[1]['content']
        if task == 'provider_error':
            self.send_response(403); self.end_headers(); self.wfile.write(b'do-not-store-this-provider-body'); return
        if task == 'stale':
            if outputs:
                assert 'changed since' in json.loads(outputs[-1]['content'])['error']
                events = [{"choices": [{"delta": {"content": "Stale edit blocked."}, "finish_reason": None}]}]
            else:
                events = call('apply_patch', {'path': 'hello.txt', 'old_text': 'hello substrate', 'new_text': 'wrong'})
        elif task == 'cancel_stream':
            self.send_response(200); self.send_header('Content-Type', 'text/event-stream'); self.end_headers()
            self.wfile.write(b'data: {"choices":[{"delta":{"content":"Started"},"finish_reason":null}]}\n\n'); self.wfile.flush()
            time.sleep(2); return

        elif task == 'length':
            events = []
        elif task == 'cancel':
            events = call('execute_command', {'command': 'sleep 60 & child=$!; echo "$child"; wait "$child"'})
        elif task == 'reject':
            if messages[-1]['role'] == 'tool':
                assert json.loads(messages[-1]['content'])['rejected']
                events = [{"choices": [{"delta": {"content": "Declined; no file was written."}, "finish_reason": None}]}]
            else:
                events = call('apply_patch', {'path': 'denied.txt', 'old_text': '', 'new_text': 'no'})
        elif not outputs:
            events = call('read_file', {'path': 'hello.txt'}) + call('list_files', {'path': ''}, 1)
        elif len(outputs) == 2:
            assert 'hello' in outputs[0]['content']
            events = call('apply_patch', {'path': 'hello.txt', 'old_text': 'hello', 'new_text': 'hello substrate'})
        elif len(outputs) == 3:
            assert json.loads(outputs[-1]['content'])['applied']
            events = call('execute_command', {'command': "printf 'verified\\n'; test -z \"$SUBQ_API_KEY\""})
        else:
            assert json.loads(outputs[-1]['content'])['exit_code'] == 0
            events = [{"choices": [{"delta": {"content": 'Done. 世界'}, "finish_reason": None}]}]
        events.append({"choices": [{"delta": {}, "finish_reason": 'length' if task == 'length' else ('tool_calls' if any(e['choices'][0]['delta'].get('tool_calls') for e in events) else 'stop')}]})
        events.append({"choices": [], "usage": {"prompt_tokens": 1000+len(requests), "completion_tokens": 25, "total_tokens": 1025+len(requests), "prompt_tokens_details": {"cached_tokens": 0}, "completion_tokens_details": {"reasoning_tokens": 5}}})
        self.send_response(200)
        self.send_header('Content-Type', 'text/event-stream')
        self.end_headers()
        try:
            if task == 'stale' and not outputs:
                self.wfile.write(b'data: {"choices":[{"delta":{"content":"Preparing edit"},"finish_reason":null}]}\n\n'); self.wfile.flush()
                assert stale_ready.wait(timeout=5)
            for event in events:
                frame = ('data: ' + json.dumps(event, ensure_ascii=False) + '\r\n\r\n').encode()
                # Deliberately split every Unicode codepoint over network writes.
                for offset in range(0, len(frame), 7):
                    self.wfile.write(frame[offset:offset+7])
                self.wfile.flush()
            self.wfile.write(b'data: [DONE]\r\n\r\n')
        except (BrokenPipeError, ConnectionResetError):
            pass

class Client:
    def __init__(self, directory, url):
        env = dict(os.environ, SUBSTRATE_DB=str(directory / 'data' / 'test.db'), SUBQ_API_KEY='fixture-key', SUBSTRATE_API_BASE=url)
        self.process = subprocess.Popen([str(ROOT / 'target/debug/agent-server')], stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True, env=env)
        self.messages = queue.Queue()
        self.events = []
        self.sequence = 0
        def reader():
            for line in self.process.stdout:
                self.messages.put(json.loads(line))
        threading.Thread(target=reader, daemon=True).start()

    def send(self, method, **params):
        self.sequence += 1
        self.process.stdin.write(json.dumps({'jsonrpc': '2.0', 'id': self.sequence, 'method': method, 'params': params}) + '\n')
        self.process.stdin.flush()
        return self.sequence

    def rpc(self, method, **params):
        id = self.send(method, **params)
        deadline = time.monotonic() + 10
        while time.monotonic() < deadline:
            message = self.messages.get(timeout=10)
            if message.get('id') == id:
                if 'error' in message:
                    raise RuntimeError(message['error']['message'])
                return message['result']
            self.events.append(message)
        raise AssertionError('RPC timeout')

    def wait(self, method, predicate=lambda params: True):
        for i, event in enumerate(self.events):
            if event.get('method') == method and predicate(event.get('params', {})):
                return self.events.pop(i)['params']
        while True:
            message = self.messages.get(timeout=10)
            if message.get('method') == method and predicate(message.get('params', {})):
                return message['params']
            self.events.append(message)

    def close(self):
        self.process.stdin.close()
        try:
            self.process.wait(timeout=5)
        except subprocess.TimeoutExpired:
            self.process.kill()
            raise
        assert self.process.returncode == 0, self.process.stderr.read()


def main():
    subprocess.run(['cargo', 'build', '-q', '-p', 'agent-server'], cwd=ROOT, check=True)
    http = ThreadingHTTPServer(('127.0.0.1', 0), Model)
    threading.Thread(target=http.serve_forever, daemon=True).start()
    url = f'http://127.0.0.1:{http.server_port}/v1'
    with tempfile.TemporaryDirectory(prefix='substrate-integration-') as tmp:
        directory = Path(tmp)
        repo = directory / 'repo'
        repo.mkdir()
        (repo / 'hello.txt').write_text('hello')
        (repo / '.env').write_text('PRIVATE_TEST_SECRET=not-for-model')
        (repo / '.gitignore').write_text('ignored.txt\n')
        (repo / 'ignored.txt').write_text('ignored-source-data')
        client = Client(directory, url)
        project = client.rpc('project.open', path=str(repo))
        session = client.rpc('session.create', project=project['path'])['id']
        baseline = client.rpc('context.get', session_id=session)['snapshot']
        assert baseline['revision'] == 1
        assert 'content' not in baseline['files'][0]
        assert not any(f['path'] in ['.env', 'ignored.txt'] for f in baseline['files'])
        assert client.rpc('context.file', session_id=session, snapshot_id=baseline['id'], path='hello.txt')['content'] == 'hello'
        assert client.rpc('context.refresh', session_id=session)['id'] == baseline['id']
        # Concurrent IDs must stay correlated despite out-of-order responses.
        first = client.send('ping'); second = client.send('config.get')
        responses = [client.messages.get(timeout=5), client.messages.get(timeout=5)]
        assert {r['id'] for r in responses} == {first, second}
        client.rpc('agent.start', session_id=session, task='build')
        approval = client.wait('tool.approval.required')['tool']
        assert approval['name'] == 'apply_patch'
        assert (repo / 'hello.txt').read_text() == 'hello'
        assert approval['preview']['after'] == 'hello substrate'
        client.rpc('tool.approve', session_id=session, tool_id=approval['id'])
        approval = client.wait('tool.approval.required')['tool']
        assert approval['name'] == 'execute_command'
        client.rpc('tool.approve', session_id=session, tool_id=approval['id'])
        client.wait('agent.completed')
        assert (repo / 'hello.txt').read_text() == 'hello substrate'
        assert any(e.get('method') == 'tool.output.delta' for e in client.events)
        saved = client.rpc('session.resume', session_id=session)
        assert saved['messages'][-1]['content'] == 'Done. 世界'
        assert len([m for m in saved['messages'] if m['role'] == 'tool']) == 4
        usage = client.rpc('usage.get', session_id=session)
        assert len(usage['calls']) == 4 and usage['runs'][0]['status'] == 'completed'
        assert sum(c['metrics']['usage']['input_tokens'] for c in usage['calls']) == 4010
        assert all(c['metrics']['usage']['cached_tokens'] == 0 for c in usage['calls'])
        assert all(c['metrics']['duration_ms'] is not None and c['metrics']['first_output_ms'] is not None for c in usage['calls'])
        assert all(c['metrics']['request_bytes'] > c['context_bytes'] for c in usage['calls'])
        assert all(c['snapshot_id'] and c['run_id'] == usage['runs'][0]['id'] for c in usage['calls'])
        assert 'fixture-key' not in json.dumps(usage)
        current = client.rpc('context.get', session_id=session)['snapshot']
        assert current['revision'] == 2
        assert client.rpc('context.file', session_id=session, snapshot_id=baseline['id'], path='hello.txt')['content'] == 'hello'
        assert client.rpc('context.file', session_id=session, snapshot_id=current['id'], path='hello.txt')['content'] == 'hello substrate'
        # Rejecting an edit must return a rejection to the model, without a write.
        denied = client.rpc('session.create', project=str(repo))['id']
        client.rpc('agent.start', session_id=denied, task='reject')
        approval = client.wait('tool.approval.required')['tool']
        client.rpc('tool.reject', session_id=denied, tool_id=approval['id'])
        client.wait('agent.completed')
        assert not (repo / 'denied.txt').exists()
        # An external edit that preserves old_text must still invalidate the model's proposal.
        stale = client.rpc('session.create', project=str(repo))['id']
        client.rpc('agent.start', session_id=stale, task='stale')
        client.wait('agent.message.delta', lambda p: p.get('session_id') == stale)
        (repo / 'hello.txt').write_text('hello substrate\nexternal')
        stale_ready.set()
        client.wait('agent.completed', lambda p: p.get('session_id') == stale)
        assert (repo / 'hello.txt').read_text() == 'hello substrate\nexternal'
        assert not any(e.get('method') == 'tool.approval.required' and e.get('params', {}).get('session_id') == stale for e in client.events)
        assert len(client.rpc('context.history', session_id=stale)) == 2
        # Provider failures and interrupted streams are traced without fabricated token counts.
        failed = client.rpc('session.create', project=str(repo))['id']
        client.rpc('agent.start', session_id=failed, task='provider_error'); client.wait('agent.failed')
        failure = client.rpc('usage.get', session_id=failed)
        assert failure['calls'][0]['status'] == 'failed' and failure['calls'][0]['metrics']['http_status'] == 403
        assert failure['calls'][0]['metrics']['usage'] is None and 'do-not-store' not in json.dumps(failure)
        limited = client.rpc('session.create', project=str(repo))['id']
        client.rpc('agent.start', session_id=limited, task='length'); client.wait('agent.failed')
        limited_trace = client.rpc('usage.get', session_id=limited)['calls'][0]
        assert limited_trace['status'] == 'failed' and limited_trace['metrics']['finish_reason'] == 'length'
        assert limited_trace['metrics']['usage']['output_tokens'] == 25
        interrupted = client.rpc('session.create', project=str(repo))['id']
        client.rpc('agent.start', session_id=interrupted, task='cancel_stream')
        client.wait('agent.message.delta', lambda p: p.get('session_id') == interrupted)
        client.rpc('agent.interrupt', session_id=interrupted); client.wait('agent.interrupted')
        trace = client.rpc('usage.get', session_id=interrupted)['calls'][0]
        assert trace['status'] == 'interrupted' and trace['metrics']['usage'] is None
        assert trace['metrics']['first_output_ms'] is not None
        # Context inspection reports stale files without changing the captured revision.
        old = client.rpc('context.get', session_id=session)
        assert old['stale'] and 'hello.txt' in old['changed_paths']
        assert old['snapshot']['id'] == current['id']
        # Cancel a running process group, not just the model request.
        cancel = client.rpc('session.create', project=str(repo))['id']
        client.rpc('agent.start', session_id=cancel, task='cancel')
        approval = client.wait('tool.approval.required')['tool']
        client.rpc('tool.approve', session_id=cancel, tool_id=approval['id'])
        client.wait('tool.started', lambda p: p.get('tool', {}).get('id') == approval['id'])
        output = client.wait('tool.output.delta', lambda p: p.get('tool_id') == approval['id'])['delta']
        while '\n' not in output:
            output += client.wait('tool.output.delta', lambda p: p.get('tool_id') == approval['id'])['delta']
        child_pid = int(output.strip())
        os.kill(child_pid, 0)
        started = time.monotonic()
        client.rpc('agent.interrupt', session_id=cancel)
        client.wait('agent.interrupted')
        assert time.monotonic() - started < 3
        if os.name == 'posix':
            # A just-killed child may briefly be a zombie awaiting the system reaper.
            state = subprocess.run(['ps', '-p', str(child_pid), '-o', 'stat='], capture_output=True, text=True).stdout.strip()
            assert not state or state.startswith('Z'), f'Cancelled child still running: {state}'
        try:
            client.rpc('files.read', session_id=session, path='../data/test.db')
            raise AssertionError('Path traversal was accepted')
        except RuntimeError as e:
            assert 'traversal' in str(e)
        with sqlite3.connect(directory / 'data' / 'test.db') as db:
            blobs = db.execute('SELECT hash, content FROM context_blobs').fetchall()
            assert len(blobs) == len({row[0] for row in blobs})
            assert not any('PRIVATE_TEST_SECRET' in row[1] or 'ignored-source-data' in row[1] for row in blobs)
            input_json = db.execute('SELECT input FROM sessions WHERE id=?', (session,)).fetchone()[0]
            assert 'Repository snapshot ' not in input_json
        client.close()
        client = Client(directory, url)
        restored = client.rpc('session.resume', session_id=session)
        assert restored['messages'] == saved['messages']
        assert client.rpc('usage.get', session_id=session) == usage
        assert client.rpc('context.file', session_id=session, snapshot_id=baseline['id'], path='hello.txt')['content'] == 'hello'
        assert 'key' not in client.rpc('config.get')
        client.close()
    http.shutdown()
    print('PASS: real harness → SubQ fixture → tools → approvals → streaming → cancellation → restore')

if __name__ == '__main__':
    main()
