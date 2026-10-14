#!/usr/bin/env python3
"""Exercise Codex's real shell tool and Shika's client without a model request.

Usage: python3 scripts/check-lead-reports.py /absolute/path/to/shika [codex]
Uses only a local synthetic Responses server, disposable HOME/CODEX_HOME and
Git repository, and a stand-in control server. This is not GUI acceptance.
"""
import concurrent.futures
import http.server
import json
import os
from pathlib import Path
import shlex
import shutil
import socket
import subprocess
import sys
import tempfile
import threading


class Provider(http.server.BaseHTTPRequestHandler):
    def log_message(self, *_args):
        pass

    def do_POST(self):
        body = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
        outputs = [i for i in body.get("input", []) if i.get("type") == "function_call_output"]
        if outputs:
            with self.server.result_lock:
                self.server.results.extend(i["output"] for i in outputs)
            item = {
                "id": "msg", "type": "message", "role": "assistant",
                "content": [{"type": "output_text", "text": "Done", "annotations": []}],
            }
        else:
            names = [t.get("name") for t in body.get("tools", [])]
            if "exec_command" not in names:
                self.send_error(400, "This check requires Codex's exec_command tool")
                return
            item = {
                "id": "tool", "type": "function_call", "call_id": "call_report",
                "name": "exec_command",
                # Default login=true is intentional: it reproduces PATH loss.
                "arguments": json.dumps({
                    "cmd": self.server.command,
                    "yield_time_ms": 1000, "max_output_tokens": 1000,
                }),
            }
        response = {
            "id": "resp_local_report", "object": "response", "status": "completed",
            "output": [item], "usage": {"input_tokens": 1, "output_tokens": 1, "total_tokens": 2},
        }
        events = [
            ("response.created", {"response": {**response, "status": "in_progress", "output": []}}),
            ("response.output_item.done", {"output_index": 0, "item": item}),
            ("response.completed", {"response": response}),
        ]
        self.send_response(200)
        self.send_header("Content-Type", "text/event-stream")
        self.end_headers()
        for sequence, (event, data) in enumerate(events):
            data.update(type=event, sequence_number=sequence)
            self.wfile.write(f"event: {event}\ndata: {json.dumps(data)}\n\n".encode())
            self.wfile.flush()


def check(binary, codex):
    with tempfile.TemporaryDirectory(prefix="shika-report-check-") as temp:
        root = Path(temp).resolve()
        repo = root / "repo"
        repo.mkdir()
        subprocess.run(["git", "init", "-q", str(repo)], check=True)
        bin_dir = root / "control space'quote" / "bin"
        bin_dir.mkdir(parents=True)
        command = bin_dir / "shika"
        command.symlink_to(binary)
        sock = socket.socket(socket.AF_UNIX)
        sock.bind(str(root / "sock"))
        sock.listen()
        reports = []
        errors = []
        stop = threading.Event()
        sock.settimeout(0.2)

        def control():
            while not stop.is_set():
                try:
                    conn, _ = sock.accept()
                except socket.timeout:
                    continue
                with conn:
                    conn.settimeout(5)
                    try:
                        request = json.loads(conn.makefile("rb").readline())
                        assert request["command"]["type"] == "report", request
                        assert request["command"]["text"] == "Synthetic worker completed", request
                        assert request["token"].startswith("synthetic-worker-"), request
                        reports.append(request["token"])
                        conn.sendall(b'{"type":"done","message":"reported"}\n')
                    except Exception as error:
                        errors.append(str(error))

        receiver = threading.Thread(target=control)
        receiver.start()
        provider = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Provider)
        provider.result_lock = threading.Lock()
        provider.results = []
        serving = threading.Thread(target=provider.serve_forever)
        serving.start()

        def worker(index):
            home = root / f"home-{index}"
            home.mkdir()
            codex_home = root / f"codex-{index}"
            codex_home.mkdir()
            env = {k: v for k, v in os.environ.items() if not k.startswith(("CODEX", "OPENAI", "SHIKA"))}
            env.update(
                HOME=str(home), CODEX_HOME=str(codex_home), SHIKA_TEST_KEY="local-only",
                SHIKA_SOCKET=str(root / "sock"), SHIKA_TOKEN=f"synthetic-worker-{index}",
                PATH=f"{bin_dir}:{env.get('PATH', '/usr/bin:/bin')}",
            )
            args = [
                codex, "exec", "--ignore-user-config", "--ignore-rules",
                "--dangerously-bypass-approvals-and-sandbox", "-m", "gpt-5",
                "-c", 'model_provider="shika_test"',
                "-c", 'model_providers.shika_test.name="Shika local report check"',
                "-c", f'model_providers.shika_test.base_url="http://127.0.0.1:{provider.server_port}/v1"',
                "-c", 'model_providers.shika_test.env_key="SHIKA_TEST_KEY"',
                "-c", 'model_providers.shika_test.wire_api="responses"',
                "Run the report shell command, then finish.",
            ]
            result = subprocess.run(args, cwd=repo, env=env, stdin=subprocess.DEVNULL,
                                    capture_output=True, text=True, timeout=45)
            assert result.returncode == 0, result.stderr[-2000:]

        try:
            provider.command = 'shika report "Synthetic worker completed"'
            worker(0)
            assert not reports, "Expected the unqualified command to fail in this login shell"
            assert any("command not found: shika" in output for output in provider.results), provider.results
            print("Reproduced: Codex's login shell loses the unqualified shika command.")
            provider.results.clear()
            provider.command = f'{shlex.quote(str(command))} report "Synthetic worker completed"'
            with concurrent.futures.ThreadPoolExecutor(max_workers=3) as pool:
                list(pool.map(worker, [1, 2, 3]))
            assert not errors, errors
            assert sorted(reports) == [f"synthetic-worker-{i}" for i in [1, 2, 3]], (reports, provider.results)
            assert len(provider.results) == 3, provider.results
            assert all("reported" in output for output in provider.results), provider.results
            print("PASS: three concurrent Codex workers reported through the real Shika client.")
        finally:
            provider.shutdown()
            provider.server_close()
            serving.join()
            stop.set()
            receiver.join()
            sock.close()


if __name__ == "__main__":
    if len(sys.argv) not in (2, 3):
        sys.exit(__doc__)
    binary = Path(sys.argv[1]).resolve(strict=True)
    codex = shutil.which(sys.argv[2] if len(sys.argv) == 3 else "codex")
    if not codex:
        sys.exit("Codex is not on PATH. Supply its absolute path as the second argument.")
    check(binary, codex)
