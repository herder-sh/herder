#!/usr/bin/env python3
"""Re-records the Claude fixtures against the real `claude`, through `herder dev record`.

Plays the herder side of each scenario: it sends exactly the lines the Claude adapter sends
(tests/claude.rs replays them against the adapter, so any drift fails there), and answers
approvals, questions and interrupts the way the tests do. Run from the repo root, with
`claude` logged in:

    cargo build -p herder && crates/herder-adapters/fixtures/claude/record.py [scenario ...]

It uses whatever config dir `claude` already uses and never reads anything inside it. On top of
the adapter's own flags it passes `--safe-mode`, so the recording carries no local plugins,
hooks, MCP servers or CLAUDE.md, and `--model haiku` to keep it cheap; neither changes the
wire format. The account's organization name, the home directory and the host name are
redacted on top of `herder dev record`'s defaults (which catch the email address); they are
learned from a throwaway `claude` first.

limit_reached.jsonl is hand-built (see its header comment) and is not recorded here.
full_access.jsonl runs in `bypassPermissions`, the adapter's flag for `full_access`.
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
CWD = "/tmp/herder-claude-fixture"
TIMEOUT = 180

# The adapter's command line (claude::command) for permission mode `ask`, plus the two
# recording-only flags described above.
CLAUDE = [
    "claude", "-p", "--input-format", "stream-json", "--output-format", "stream-json",
    "--verbose", "--include-partial-messages", "--replay-user-messages",
    "--permission-prompt-tool", "stdio",
    "--allow-dangerously-skip-permissions", "--permission-mode", "default",
    "--safe-mode", "--model", "haiku",
]

# Scenarios recorded in another permission mode than `default`.
MODES = {"full_access": "bypassPermissions"}

SEED_PREAMBLE = (
    "This session continues an earlier conversation, replayed below from herder's log. "
    "It is context only and needs no reply."
)


class Claude:
    def __init__(self, command):
        self.process = subprocess.Popen(
            command, cwd=CWD, stdin=subprocess.PIPE, stdout=subprocess.PIPE, text=True, bufsize=1
        )
        self.lines = queue.Queue()
        self.next_request = 0
        threading.Thread(target=self._read, daemon=True).start()

    def _read(self):
        for line in self.process.stdout:
            self.lines.put(line)
        self.lines.put(None)

    def send(self, message):
        line = json.dumps(message, separators=(",", ":"), ensure_ascii=False)
        self.process.stdin.write(line + "\n")
        self.process.stdin.flush()

    def until(self, done):
        while True:
            line = self.lines.get(timeout=TIMEOUT)
            if line is None:
                raise SystemExit("claude closed its output")
            try:
                message = json.loads(line)
            except json.JSONDecodeError:
                continue
            if done(message):
                return message

    def request(self, request):
        """A control request as the adapter numbers them; returns its id."""
        self.next_request += 1
        request_id = f"herder-{self.next_request}"
        self.send({"type": "control_request", "request_id": request_id, "request": request})
        return request_id

    def call(self, request):
        request_id = self.request(request)
        return self.until(
            lambda m: m.get("type") == "control_response"
            and m["response"]["request_id"] == request_id
        )

    def prompt(self, text):
        self.send({
            "type": "user",
            "message": {"role": "user", "content": text},
            "parent_tool_use_id": None,
            "session_id": "",
            "origin": {"kind": "human"},
        })

    def answer(self, request_id, permission):
        self.send({
            "type": "control_response",
            "response": {"subtype": "success", "request_id": request_id, "response": permission},
        })

    def close(self):
        self.process.stdin.close()
        self.process.wait(timeout=60)


def is_result(message):
    return message.get("type") == "result"


def is_permission(message):
    return (
        message.get("type") == "control_request"
        and message["request"].get("subtype") == "can_use_tool"
    )


def scenario_turn(claude):
    claude.call({"subtype": "initialize"})
    claude.prompt("Reply with the word ok.")
    claude.until(is_result)


def scenario_switch(claude):
    claude.call({"subtype": "initialize"})
    claude.call({"subtype": "set_model", "model": "sonnet"})
    claude.call({"subtype": "set_permission_mode", "mode": "acceptEdits"})
    claude.prompt("Reply with the word ok.")
    claude.until(is_result)


def scenario_approval(claude):
    target = os.path.join(CWD, "herder-ok.txt")
    if os.path.exists(target):
        os.remove(target)
    claude.call({"subtype": "initialize"})
    claude.prompt(
        "Run the shell command `touch herder-ok.txt` with the Bash tool, then reply with the "
        "word done."
    )
    while True:
        message = claude.until(lambda m: is_result(m) or is_permission(m))
        if is_result(message):
            return
        claude.answer(message["request_id"], {"behavior": "allow"})


FULL_ACCESS = (
    "Run these two shell commands with the Bash tool, verbatim, as two separate calls in "
    "order, then reply with the word done. First: `cd sub` Second: "
    "`cd /tmp/herder-claude-fixture && rm -f sub/*; ls sub`"
)


def scenario_full_access(claude):
    """bypassPermissions still asks when a safety check holds a command: here the dangerous
    rm check, which resolves `sub/*` against the shell's cwd from before the `cd`."""
    os.makedirs(os.path.join(CWD, "sub"), exist_ok=True)
    open(os.path.join(CWD, "sub", "a.tmp"), "w").close()
    claude.call({"subtype": "initialize"})
    claude.prompt(FULL_ACCESS)
    while True:
        message = claude.until(lambda m: is_result(m) or is_permission(m))
        if is_result(message):
            return
        claude.answer(message["request_id"], {"behavior": "allow"})


QUESTION = (
    "Use the AskUserQuestion tool to ask me whether to print A or B, then reply with exactly "
    "the letter I chose."
)


def is_question(message):
    return is_permission(message) and message["request"]["tool_name"] == "AskUserQuestion"


def scenario_question(claude):
    claude.call({"subtype": "initialize"})
    claude.prompt(QUESTION)
    while True:
        message = claude.until(lambda m: is_result(m) or is_question(m))
        if is_result(message):
            return
        # The adapter's answer: the tool's own input plus `answers`, picking the second option
        # of every question. serde_json sorts object keys, so the input is sorted to match.
        tool_input = message["request"]["input"]
        answers = {q["question"]: q["options"][1]["label"] for q in tool_input["questions"]}
        updated = json.loads(json.dumps({**tool_input, "answers": answers}, sort_keys=True))
        claude.answer(message["request_id"], {"behavior": "allow", "updatedInput": updated})


def scenario_question_interrupt(claude):
    claude.call({"subtype": "initialize"})
    claude.prompt(QUESTION)
    claude.until(is_question)
    claude.request({"subtype": "interrupt"})
    claude.until(is_result)


def scenario_interrupt(claude):
    claude.call({"subtype": "initialize"})
    claude.prompt("Count from 1 to 300, one number per line.")
    claude.until(
        lambda m: m.get("type") == "stream_event"
        and m.get("parent_tool_use_id") is None
        and m["event"].get("type") == "content_block_delta"
        and m["event"]["delta"].get("type") == "text_delta"
        and m["event"]["delta"].get("text")
    )
    claude.request({"subtype": "interrupt"})
    claude.until(is_result)


def scenario_seed(claude):
    claude.call({"subtype": "initialize"})
    seed = f"{SEED_PREAMBLE}\n\nUser: My favourite colour is teal.\n\nAssistant: Noted."
    claude.send({
        "type": "user",
        "message": {"role": "user", "content": seed},
        "parent_tool_use_id": None,
        "session_id": "",
        "shouldQuery": False,
    })
    claude.until(is_result)
    claude.prompt("What is my favourite colour? Reply with one word.")
    claude.until(is_result)


SCENARIOS = {
    "turn": scenario_turn,
    "switch": scenario_switch,
    "approval": scenario_approval,
    "question": scenario_question,
    "question_interrupt": scenario_question_interrupt,
    "interrupt": scenario_interrupt,
    "seed": scenario_seed,
    "full_access": scenario_full_access,
}


def private_values():
    """Values to redact that the default patterns do not catch."""
    claude = Claude(CLAUDE)
    account = claude.call({"subtype": "initialize"})["response"]["response"].get("account", {})
    claude.close()
    values = {socket.gethostname(), os.path.expanduser("~")}
    values.update(v for v in [account.get("organization")] if v)
    return sorted(values)


def main():
    os.makedirs(CWD, exist_ok=True)
    version = subprocess.run(
        ["claude", "--version"], capture_output=True, text=True, check=True
    ).stdout.split()[0]
    redact = []
    for value in private_values():
        redact += ["--redact", re.escape(value)]
    for name in sys.argv[1:] or SCENARIOS:
        mode = MODES.get(name, "default")
        command = [mode if arg == "default" else arg for arg in CLAUDE]
        claude = Claude(
            [os.path.join(os.getcwd(), "target/debug/herder"), "dev", "record", "claude", name,
             "--out", os.path.join(HERE, f"{name}.jsonl"), "--cli-version", version,
             *redact, "--", *command]
        )
        SCENARIOS[name](claude)
        claude.close()
        print(f"recorded {name}", file=sys.stderr)


if __name__ == "__main__":
    main()
