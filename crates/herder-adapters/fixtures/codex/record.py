#!/usr/bin/env python3
"""Re-records the Codex fixtures against the real `codex app-server`, through `herder dev record`.

Plays the herder side of each scenario: it sends exactly the lines the Codex adapter sends
(tests/codex.rs replays them against the adapter, so any drift fails there), and answers
approvals and interrupts the way the tests do. Run from the repo root, with `codex` logged in:

    cargo build -p herder && crates/herder-adapters/fixtures/codex/record.py [scenario ...]

It uses whatever CODEX_HOME `codex` already uses and never reads anything inside it. The
account and installation ids, the host name and the home directory are redacted on top of
`herder dev record`'s defaults; they are learned from a throwaway app-server first.

limit_reached.jsonl is hand-built (see its header comment) and is not recorded here.
"""

import json
import os
import queue
import re
import socket
import subprocess
import sys
import threading

HERE = os.path.dirname(os.path.abspath(__file__))
CWD = "/tmp/herder-codex-fixture"
TIMEOUT = 120


class Server:
    def __init__(self, command):
        self.process = subprocess.Popen(
            command, cwd=CWD, stdin=subprocess.PIPE, stdout=subprocess.PIPE, text=True, bufsize=1
        )
        self.lines = queue.Queue()
        self.seen = []
        self.next_id = 0
        threading.Thread(target=self._read, daemon=True).start()

    def _read(self):
        for line in self.process.stdout:
            self.lines.put(line)
        self.lines.put(None)

    def send(self, message):
        self.process.stdin.write(json.dumps(message, separators=(",", ":")) + "\n")
        self.process.stdin.flush()

    def request(self, method, params):
        request_id = self.next_id
        self.next_id += 1
        self.send({"id": request_id, "method": method, "params": params})
        return request_id

    def until(self, done):
        while True:
            line = self.lines.get(timeout=TIMEOUT)
            if line is None:
                raise SystemExit("app-server closed its output")
            message = json.loads(line)
            self.seen.append(message)
            if done(message):
                return message

    def call(self, method, params):
        request_id = self.request(method, params)
        return self.until(lambda m: m.get("id") == request_id and "method" not in m)

    def close(self):
        self.process.stdin.close()
        self.process.wait(timeout=30)


def start(server, model=None, approval="untrusted", sandbox="read-only", seed=()):
    """The adapter's startup sequence; returns the thread id."""
    server.call(
        "initialize",
        {"clientInfo": {"name": "herder", "title": None, "version": "0.0.0"}, "capabilities": None},
    )
    server.send({"method": "initialized"})
    server.call("account/read", {"refreshToken": False})
    server.call("account/rateLimits/read", None)
    params = {"cwd": CWD, "approvalPolicy": approval, "sandbox": sandbox}
    if model:
        params = {"model": model, **params}
    thread = server.call("thread/start", params)["result"]["thread"]["id"]
    if seed:
        server.call("thread/inject_items", {"threadId": thread, "items": list(seed)})
    return thread


def turn(server, thread, text, model=None, approval="untrusted", sandbox=None):
    params = {
        "threadId": thread,
        "input": [{"type": "text", "text": text, "text_elements": []}],
    }
    if model:
        params["model"] = model
    params["approvalPolicy"] = approval
    params["sandboxPolicy"] = sandbox or {"type": "readOnly", "networkAccess": False}
    return server.request("turn/start", params)


def is_turn_end(message):
    return message.get("method") == "turn/completed"


def scenario_turn(server):
    thread = start(server)
    turn(
        server,
        thread,
        "Reply with the word ok.",
        model="gpt-6-luna",
        approval="never",
    )
    server.until(is_turn_end)


def scenario_approval(server):
    thread = start(server)
    turn(server, thread, "Run the shell command: touch herder-ok.txt")
    while True:
        message = server.until(lambda m: is_turn_end(m) or ("id" in m and "method" in m))
        if is_turn_end(message):
            return
        server.send({"id": message["id"], "result": {"decision": "accept"}})


def scenario_interrupt(server):
    thread = start(server)
    turn_request = turn(server, thread, "Count from 1 to 300, one number per line.")
    turn_id = server.until(lambda m: m.get("id") == turn_request)["result"]["turn"]["id"]
    server.until(lambda m: m.get("method") == "item/agentMessage/delta")
    server.request("turn/interrupt", {"threadId": thread, "turnId": turn_id})
    server.until(is_turn_end)


def message(role, kind, text):
    return {"type": "message", "role": role, "content": [{"type": kind, "text": text}]}


def scenario_seed(server):
    thread = start(
        server,
        seed=[
            message("user", "input_text", "My favourite colour is teal."),
            message("assistant", "output_text", "Noted."),
        ],
    )
    turn(server, thread, "What is my favourite colour? Reply with one word.")
    server.until(is_turn_end)


SCENARIOS = {
    "turn": scenario_turn,
    "approval": scenario_approval,
    "interrupt": scenario_interrupt,
    "seed": scenario_seed,
}


def private_values():
    """Values to redact that the default patterns do not catch."""
    server = Server(["codex", "app-server"])
    start(server)
    account = server.call("account/read", {"refreshToken": False})["result"]
    limits = server.call("account/rateLimits/read", None).get("result") or {}
    values = {socket.gethostname(), os.path.expanduser("~")}
    routing = account.get("workspaceRouting") or {}
    values.update(v for v in [routing.get("chatgptAccountId"), limits.get("accountId")] if v)
    # The per-machine installation id comes with remoteControl/status/changed.
    values.update(
        m["params"]["installationId"]
        for m in server.seen
        if m.get("params", {}).get("installationId")
    )
    server.close()
    return sorted(values)


def main():
    os.makedirs(CWD, exist_ok=True)
    version = subprocess.run(
        ["codex", "--version"], capture_output=True, text=True, check=True
    ).stdout.split()[-1]
    redact = []
    for value in private_values():
        redact += ["--redact", re.escape(value)]
    for name in sys.argv[1:] or SCENARIOS:
        server = Server(
            [os.path.join(os.getcwd(), "target/debug/herder"), "dev", "record", "codex", name,
             "--out", os.path.join(HERE, f"{name}.jsonl"), "--cli-version", version,
             "--ignore-key", "version", *redact, "--", "codex", "app-server"]
        )
        SCENARIOS[name](server)
        server.close()
        print(f"recorded {name}", file=sys.stderr)


if __name__ == "__main__":
    main()
