# AGENTS.md

Working rules for every agent and human contributing to herder. Many agents work on this
repo in parallel; these rules keep that safe. Follow them exactly.

## Work unit

- One Basecamp todo = one branch = one PR.
- Branch name: `p<phase>-<n>-<slug>`, e.g. `p1-6-claude-adapter`.
- PR title starts with the todo code: `P1.6 · Claude adapter`.
- PR body links the Basecamp todo URL and restates its "Done when", with how you verified it.
- Branch from the latest `origin/main`, using `git worktree add`. Rebase before you push.

## Stay in your lane

- Touch only the crates and directories your todo names.
- Public contracts change only in their own `[CONTRACT]` todo. Never change one in passing:
  - anything in `herder-protocol`
  - the `herder-store` schema
  - the adapter trait in `herder-adapters`
  - the public API of `herder-client-core`
- If your todo cannot be done without a contract change, stop and say so in the PR.

## Crates

| Crate                | Responsibility                                               |
| -------------------- | ------------------------------------------------------------ |
| `herder-protocol`    | Wire/protocol types and their JSON Schema                    |
| `herder-store`       | SQLite append-only event journal and projections             |
| `herder-adapters`    | Provider adapter trait, vendor adapters, fake adapter        |
| `herder-daemon`      | Daemon runtime, WebSocket server, sessions                   |
| `herder-client-core` | Shared client core for the TUI and native apps               |
| `herder-tui`         | ratatui client                                               |
| `herder`             | The single binary: `herder daemon`, bare `herder` = TUI      |

Non-Rust clients live in `apple/`, `android/`, `linux/`; vault manifests in `deploy/`.

## Done

Done = the `ci` check is green and every review thread is resolved.

Run locally before pushing:

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

## Code rules

- No `#[allow(...)]` to silence clippy without a comment explaining why.
- No `unwrap()` / `expect()` in non-test code, unless a comment proves it cannot fail.
- `unsafe_code` is forbidden workspace-wide. Overriding it in a crate needs its own todo.
- New dependencies go in `[workspace.dependencies]`; crates use `dep.workspace = true`.
- Every crate inherits `[lints] workspace = true`.

## Tests

- Use the fake adapter and recorded fixtures.
- Never call a real provider (Claude, Codex, Cursor, ...) in CI or in tests.
- Every change that adds behaviour adds a test that fails without it.

## Product rules (never break)

- Never read, store, copy or relay provider login tokens (Claude, Gemini, or any other).
  herder only drives the unmodified vendor CLI under a per-account config dir.
- Terminals are owner-only: only users with the owner role on that daemon can open, attach to
  or see one. Members can drive sessions but never get a shell.
- Failover is reactive-only and opt-in per account. Never switch accounts pre-emptively.

## Engineering principles

- Build the simplest implementation that fully meets the current requirements.
- No speculative abstractions, configuration or indirection.
- No backward-compatibility shims. herder is pre-1.0: remove obsolete paths.
- Prefer well-maintained crates over reimplementing common functionality.
- Grow in layers: each change lands on a product that already works end to end.

## Brand

Write the name as lowercase `herder` in all user-facing text: CLI output, help text, UI,
docs, error messages. Rust identifiers and crate names follow Rust conventions.

## Commits

- Imperative subject line, under ~72 characters: `Add fake adapter replay`.
- Body explains why the change is needed, not what the diff already shows.
