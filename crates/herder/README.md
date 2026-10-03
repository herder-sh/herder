# herder (the binary)

The single `herder` binary: `herder daemon` runs the daemon, bare `herder` opens the TUI.
`herder --help` lists every command.

## Scripting sessions

`herder session` drives sessions from scripts and other agents, through the same client
profile as the TUI (pair a machine first with `herder connect`). It never prompts; every
command takes `--json` for one JSON value on stdout, and `--machine <name or host id>`,
which defaults to the only paired machine.

```sh
id=$(echo "Fix the flaky test" | herder session new --repo /src/app --account work \
  --mode auto-edit --branch fix-flaky)
herder session wait "$id" --timeout 3600 --json
echo "Now open a PR" | herder session send "$id"
herder session send "$id" --approve <approval id>      # or --deny <approval id>
herder session send "$id" --answer <question id> 2     # a choice's text or number, or free text
herder session status "$id" --json                     # includes linked PRs: number, state, ci
herder session list --json
herder session archive "$id"
herder session unarchive "$id"
```

| Command   | Does                                                                                     |
| --------- | ---------------------------------------------------------------------------------------- |
| `new`     | Creates a session (`--repo`, `--account` or `--provider`, `--model`, `--mode`, `--branch`), prompts it with stdin, prints its id |
| `send`    | Prompts with stdin, queued behind a running turn as in the TUI; or `--approve`, `--deny`, `--answer` |
| `wait`    | Blocks until the session is idle, needs you or failed; prints its status, last reply and open requests |
| `status`  | Status, repo, branch, model, account, mode, linked PRs, open requests, last reply        |
| `list`    | Every session of the machine, as `status` shows them                                     |
| `archive` | Removes the worktree, keeps the branches, makes the session read-only (`--force`)        |
| `unarchive` | Adds the worktree back on the session's branch and makes the session writable again   |

`--account` takes an account id or label and defaults to the machine's only account;
`--provider` instead picks the account of that provider with the most room left. `--mode` is
one of `read-only`, `ask`, `auto-edit`, `full-access`, and defaults to the project's default
mode, else `ask`.

Exit codes: 0 done, 1 failed (the reason is on stderr), 64 usage error. `wait` also exits 2
when the session needs you (an approval, a question, or a failed turn), 3 when it is in error,
and 4 when `--timeout` passed first.

Switch a session with `herder session switch <id> --account <id-or-label>`,
`--provider <provider>`, or `--model <model>`. A provider switch picks its account
with the most reported quota left; `--account` can choose one explicitly, and
`--model` can accompany either switch. These use the existing switch commands;
a running turn must finish or be interrupted before changing accounts.

After a usage limit with no failover target, sessions with a known future reset
wait automatically. `session status`, `wait`, and `list` show “waiting for limit
reset” and the time in UTC; JSON retains `waiting_for_capacity` and adds
`retry_at`. The queued retry survives a daemon restart. Sending a prompt,
successfully switching account/provider/model, or interrupting cancels that retry.
Unknown reset times still need user action.
