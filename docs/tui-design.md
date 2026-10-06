# herder TUI design

Status: proposal for P2d (owner review, D6). This document adds no code.
Implementation is P2d.2 onwards.

Owner's direction: *"make it similar to Herdr for navigation and stuff, and OpenCode for
interacting with the agent — they have the best decisions already made."* This spec takes
that literally:

- **Herdr shapes the frame**: the sidebar, how agent state rolls up, the prefix key and
  navigate mode, the mouse, and the phone layout.
- **OpenCode shapes the session pane**: messages, tool calls, the prompt editor, pickers,
  approvals, toasts, themes and keybind names.
- **herder's own concepts decide what goes in the frame**: machines, projects, sessions,
  task trees, the inbox, PRs, accounts, terminals and the fleet.

Credits: [Herdr](https://github.com/herdrdev/herdr) (Apache-2.0) and
[OpenCode](https://github.com/anomalyco/opencode) (MIT). We studied their source and docs; no
code, theme file or asset is copied. Wherever a name below is OpenCode's (a theme token, a
keybind name), that is deliberate, so OpenCode users feel at home.

**Reading guide (about 15 minutes):** read §1 for the decisions, look over the mockups in
§2, and skim the rest.

---

## 1. Decisions at a glance

1. **One frame for everything.** The frame is a sidebar on the left (projects ▸ sessions ▸
   task children, then an *attention* list) and a main pane on the right. Screens at least
   120 columns wide also get a details panel on the far right. Inbox, PRs, accounts and
   fleet are views shown in the main pane, not separate screens. (Herdr's spaces and agents
   sections; OpenCode's sidebar.)
2. **State is a glyph plus a colour, never colour alone, and it rolls up.** A child's state
   rolls up to its session, a session's to its project, and so on up to the machine. The
   roll-up priority is
   `needs you > error > done > running > waiting > idle`.
   "Done" means *finished since you last looked*, tracked per client, which is Herdr's best
   idea.
3. **Two input modes, as in Herdr:**
   - PROMPT: keys go to the editor.
   - NAVIGATE: bare letters are commands.

   `Esc` (or `⌫` on an empty prompt) switches from PROMPT to NAVIGATE. `i` or `Enter`
   switches back. Opening a session puts you in PROMPT, as OpenCode does.
4. **Leader key `ctrl+x`** (OpenCode's leader). `ctrl+x <key>` does exactly what `<key>` does
   in NAVIGATE, so there is only one keymap to learn. Herdr works the same way: prefix
   actions also work bare in navigate mode. The leader times out after 2 s.
5. **Today's letter keys keep working.** NAVIGATE keeps every bare key the current TUI has
   (`j k g G n s t m A R I p P L z v`, digits for answers, `y`/`n`), so existing users lose
   nothing.
6. **OpenCode's chat rendering.**
   - The user's message has a coloured left bar. Assistant text has no frame.
   - Each tool call is one dense line, which grows into a framed block when it has output or
     a diff.
   - Reasoning is folded into one line.
   - Each reply ends with a footer line.
7. **Approvals and questions replace the prompt** inline rather than opening a modal, and
   are answered with `y`/`n` or a digit. This is OpenCode's permission panel with herder's
   two outcomes (allow or deny; there is no "always").
8. **`/` commands and `@` mentions in the prompt.** These replace today's `:` palette;
   `:` still opens it in NAVIGATE. `ctrl+p` opens a command palette listing everything.
9. **Phone (≤ 64 columns).** This follows Herdr's mobile redesign rather than a squeezed
   desktop:
   - A two-row header with an attention summary and a **switch** button.
   - A full-screen switcher.
   - The current button bar.
   - The ASCII glyph set by default.
10. **Themes use OpenCode's token names**, plus herder state tokens. There are two
    built-ins, `herder` (dark and light) and `ansi` (16 named colours). `ansi` is chosen
    automatically when the terminal does not report truecolor, which covers mosh and some
    phone apps.

---

## 2. Mockups

The mockups use the Unicode glyph set at 100 and 160 columns and the ASCII set at 45.
Bold, colours and the selected-row background can't be shown in plain text, so `[chat]`
marks the active tab and `▶`/`>` marks the cursor. Phone mockups are 44 wide because the
last column is never drawn (P2.15).

### 2.1 Session (the main screen)

```text
 herder         inbox 2 «│ [chat]  tasks 2  prs 1  term                    claude-main · opus · ask
                         │
 projects                │ ┃ Add a health endpoint and test it.
 ● app                 1 │
   ├ ✓ fix-login         │   + Thought: where the router lives · 4s
   └ ● api               │   → Read src/api.rs
     ├ ● write tests     │   ✱ Grep "Router::new" in src (3 matches)
     └ ◉ docs          ? │
 ● herder                │ ┃ # cargo test --workspace            (in ~/src/app · wt api)
   └ ● p2d-1-design      │ ┃ running 12 tests
 ○ infra                 │ ┃ test health::ok ... ok
                         │ ┃ … 9 more lines                                           e expand
 ─────────────────────── │
 attention      priority │   ← Edit src/api.rs  +12 −1
                         │   ◇ Task write tests → write-tests   ↳ 4 tool calls · 1m 02s
 ● api · app · box       │
 ◉ docs · app · box      │   I added GET /health; it returns 200 with the build version so
 ✓ fix-login · app · box │   load balancers can probe it. Removing the old target next.
 ● p2d-1 · herder · m2   │
 ● write tests · app     │ ┃ write the docs page too, and link it from @README.md▌
                         │ ┃
                         │ ┃ claude-main · opus · ask
 ─────────────────────── │ ╹▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀
 + new            ≡ menu │ ⠹ working · 1m 12s  ctrl+c stop                       claude-main 5h 38%
─────────────────────────┴──────────────────────────────────────────────────────────────────────────
 PROMPT  enter send  shift+enter newline  / commands  @ mention                  ● box ● m2 ◌ vault
```

How the screen is built:

- **Sidebar:**
  - **projects** is the tree: project ▸ sessions ▸ task children. Every row shows its
    rolled-up state. The number on a project or session row is how many sessions below it
    need you.
  - Task trees stay whole. With archived sessions hidden (`H` shows them), only a tree whose
    every session is archived is hidden. An archived parent with live children stays, muted,
    as their root, and a live parent lists its archived children, muted, after its live ones.
  - **attention** lists every session flat, sorted by priority (needs you, then done, then
    running…), with `title · project · machine`. You can toggle this to `grouped`, as in
    Herdr.
- **Main pane:**
  - The **tabs** are `chat`, `tasks N`, `prs N` and `term` (the last for owners only). The
    right of the tab row shows `account · model · mode`.
  - **The prompt** grows from 1 line up to `max(6, height/3)` lines. Its meta line repeats
    `account · model · mode`.
  - **The status line** under the prompt shows a spinner while a turn runs, plus usage of
    the current account's busiest window.
- **Bottom bar:** a mode badge, the keys that work right now, and every machine's
  connection.

The same moment with an approval pending: the panel replaces the prompt.

```text
 herder         inbox 2 «│ [chat]  tasks 2  prs 1  term                    claude-main · opus · ask
                         │
 projects                │ ┃ Add a health endpoint and test it.
 ◉ app                 2 │
   ├ ✓ fix-login         │   + Thought: where the router lives · 4s
   └ ◉ api             1 │   → Read src/api.rs
     ├ ● write tests     │   ✱ Grep "Router::new" in src (3 matches)
     └ ◉ docs          ? │
 ● herder                │ ┃ # cargo test --workspace            (in ~/src/app · wt api)
   └ ● p2d-1-design      │ ┃ running 12 tests
 ○ infra                 │ ┃ test health::ok ... ok
                         │ ┃ … 9 more lines                                           e expand
 ─────────────────────── │
 attention      priority │   ← Edit src/api.rs  +12 −1
                         │   ◇ Task write tests → write-tests   ↳ 4 tool calls · 1m 02s
 ◉ api · app · box       │
 ◉ docs · app · box      │   I added GET /health; it returns 200 with the build version so
 ✓ fix-login · app · box │   load balancers can probe it. Removing the old target next.
 ● p2d-1 · herder · m2   │
 ● write tests · app     │ ┃ △ approval · Bash                          asked 12s ago
                         │ ┃ $ rm -rf target/
                         │ ┃
 ─────────────────────── │ ┃  [ allow ]   deny                 y allow · n deny · ←/→ enter
 + new            ≡ menu │
─────────────────────────┴──────────────────────────────────────────────────────────────────────────
 APPROVAL  y allow  n deny  ←/→ choose  enter confirm  esc navigate              ● box ● m2 ◌ vault
```

At **160 columns** a details panel (42 columns, OpenCode's sidebar) opens to the right. Its
sections are, in order:

- session: branch, machine and path
- account usage
- tasks
- PRs
- resources
- terminals

A section with more than two rows folds with `▼`/`▶`. Under 120 columns the panel opens as
an overlay with `ctrl+x d`.

```text
 herder              inbox 2 «│ [chat]  tasks 2  prs 1  term                                                         │ api
                              │                                                                                      │ herder/api · box · ~/src/app
 projects          by project │ ┃ Add a health endpoint and test it.                                                 │ claude-main · opus · ask
 ◉ app            box · m2  2 │                                                                                      │
   ├ ✓ fix-login              │   + Thought: where the router lives · 4s                                             │ Usage · claude-main
   └ ● api              #12 ✓ │   → Read src/api.rs                                                                  │ 5h    ███████░░░░░░░░░░ 38%
     ├ ● write tests          │   ✱ Grep "Router::new" in src (3 matches)                                            │ week  ██░░░░░░░░░░░░░░░ 12%
     └ ◉ docs               ? │                                                                                      │
 ● herder                  m2 │ ┃ # cargo test --workspace            (in ~/src/app · wt api)                        │
   └ ● p2d-1-design     #88 … │ ┃ running 12 tests                                                                   │ ▼ Tasks 2 · max 4
 ○ infra                  box │ ┃ test health::ok ... ok                                                             │ ● write tests   ↳ Bash cargo test
                              │ ┃ … 9 more lines                                           e expand                  │ ◉ docs          ? which heading level
 ──────────────────────────── │                                                                                      │
 attention           priority │   ← Edit src/api.rs  +12 −1                                                          │ ▼ Pull requests
                              │   ◇ Task write tests → write-tests   ↳ 4 tool calls · 1m 02s                         │ #12 open  ci ✓  review …  merge ✓
 ◉ docs · app             box │                                                                                      │     Add health endpoint
 ✓ fix-login · app        box │   I added GET /health; it returns 200 with the build version so                      │
 ● api · app              box │   load balancers can probe it. Removing the old target next.                         │ ▼ Resources
 ● p2d-1 · herder          m2 │                                                                                      │ cpu 23%  mem 1.2G  procs 9
 ● write tests · app      box │ ┃ write the docs page too, and link it from @README.md▌                              │ ● postgres  ● redis
                              │ ┃                                                                                    │
                              │ ┃ claude-main · opus · ask                                                           │ ▶ Terminals 1
 ──────────────────────────── │ ╹▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀ │
 + new                 ≡ menu │ ⠹ working · 1m 12s  ctrl+c stop                      ctrl+x leader · ctrl+p commands │ ⠹ working 1m 12s                  5h 38%
──────────────────────────────┴──────────────────────────────────────────────────────────────────────────────────────┴──────────────────────────────────────────
 PROMPT  enter send  shift+enter newline  / commands  @ mention  esc navigate                                                  ● box  ● m2  ◌ vault (2/3 hosts)
```

At **45 columns** (phone) there is no sidebar and no tabs; the layout is Herdr's mobile
model.

- Row 1 shows the current session `title - project - machine`, its position, and the
  **switch** button.
- Row 2 summarises attention across everything. When anything needs you, the button
  carries a `!` (in ASCII; `◉` in Unicode).
- The button bar at the bottom is today's, with keys shown as labels. `[y allow]` is the
  button that `tab` (a Termius swipe) has focused, so approving is `tab`, `Enter` (P2.16).

```text
 * api - app - box              2/6 │ switch
 ! 2 need you - 3 running - 1 done    │   !
────────────────────────────────────────────
 │ Add a health endpoint and test it.

   + Thought: where the router lives - 4s
   > Read src/api.rs

 │ # cargo test --workspace
 │ running 12 tests
 │ . 10 more lines              e expand

   < Edit src/api.rs  +12 -1

   I added GET /health; it returns 200
   with the build version so load
   balancers can probe it.

 │ ^ approval - Bash
 │ $ rm -rf target/
 │
 │  [ allow ]   deny

 [y allow] n deny  < back  ^c stop  : cmd
```

Writing a prompt on a phone: the `@` popup opens *above* the prompt, as in OpenCode.

```text
 * api - app - box              2/6 │ switch
 ! 2 need you - 3 running - 1 done    │   !
────────────────────────────────────────────
 │ Add a health endpoint and test it.

   + Thought: where the router lives - 4s
   > Read src/api.rs

 │ # cargo test --workspace
 │ running 12 tests
 │ . 10 more lines              e expand

   < Edit src/api.rs  +12 -1

   I added GET /health; it returns 200
   with the build version so load
   balancers can probe it.

 │ write the docs page too, and link
 │ it from @READ_
 ┌─────────────────────────────────────┐
 │ @README.md                          │
 │ @docs/README.md                     │
 └─────────────────────────────────────┘
 > send  esc done  / cmd  @ file
```

A question replaces the prompt in the same way an approval does:

```text
 * api - app - box              2/6 │ switch
 ! 2 need you - 3 running - 1 done    │   !
────────────────────────────────────────────
 │ Add a health endpoint and test it.

   + Thought: where the router lives - 4s
   > Read src/api.rs

 │ # cargo test --workspace
 │ running 12 tests
 │ . 10 more lines              e expand

   < Edit src/api.rs  +12 -1

   I added GET /health; it returns 200

 │ ? question from docs
 │ Which heading level for the API page?
 │  1 h2 under Reference
 │  2 h1, its own page
 │  or type an answer: _

 1 pick  2 pick  enter type  < back
```

### 2.2 Switcher, go-to and the session list on a phone

The `switch` button, `/` in NAVIGATE, or `ctrl+x g` opens the switcher. On a phone it is
full-screen (Herdr's mobile navigate). Its sections are attention, projects, views and menu.

```text
 switch                             close x
────────────────────────────────────────────
 attention
 ! docs - app - box
 v fix-login - app - box
 * api - app - box
 * p2d-1 - herder - m2

 projects
 ! app                              box m2
   v fix-login
   ! api                                 1
     * write tests
     ! docs                              ?
 * herder                               m2
 o infra                               box
 + new session

 views
 inbox 2   prs 3   accounts   fleet

 menu
 settings  keys  reconnect  quit
```

On wider screens the same list is the **go to** dialog. It searches sessions, projects,
machines and views; filters are `s`, `p` and `m` when the search box is empty. The dialog
is 60 columns wide, centred, and closes with `esc`.

```text
 herder         inbox 2 «│ [chat]  tasks 2  prs 1  term                    claude-main · opus · ask
                         │
 projects                │ ┃ Add a health endpoint and test it.
 ● app                 1 │
   ├ ✓ fix-login         │   + Th┌─ go to ──────────────────────────────────────────── esc ─┐
   └ ● api               │   → Re│ / docs▌                                                  │
     ├ ● write tests     │   ✱ Gr│                                                          │
     └ ◉ docs          ? │       │ sessions                                                 │
 ● herder                │ ┃ # ca│ ▶ ◉ docs           app › api › docs        box           │
   └ ● p2d-1-design      │ ┃ runn│   ● p2d-1-design   herder                  m2            │
 ○ infra                 │ ┃ test│ views                                                    │
                         │ ┃ … 9 │   inbox  ·  prs  ·  accounts  ·  fleet                   │
 ─────────────────────── │       │ filters  s sessions  p projects  m machines              │
 attention      priority │   ← Ed└──────────────────────────────────────────────────────────┘
                         │   ◇ Task write tests → write-tests   ↳ 4 tool calls · 1m 02s
 ● api · app · box       │
 ◉ docs · app · box      │   I added GET /health; it returns 200 with the build version so
 ✓ fix-login · app · box │   load balancers can probe it. Removing the old target next.
 ● p2d-1 · herder · m2   │
 ● write tests · app     │ ┃ write the docs page too, and link it from @README.md▌
                         │ ┃
                         │ ┃ claude-main · opus · ask
 ─────────────────────── │ ╹▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀
 + new            ≡ menu │ ⠹ working · 1m 12s  ctrl+c stop                       claude-main 5h 38%
─────────────────────────┴──────────────────────────────────────────────────────────────────────────
 PROMPT  enter send  shift+enter newline  / commands  @ mention                  ● box ● m2 ◌ vault
```

### 2.3 Inbox

The inbox lists every approval and question routed to you, across machines, newest first.
Each entry shows the task path, the escalation reason and the primary's note. Answering
happens in place.

```text
 herder         INBOX 2 «│ inbox · 2 waiting on you                    every machine · newest first
                         │
 projects                │ ▶ ◉ question · docs  (app › api › docs)                    box · 2m
 ◉ app                 2 │     escalated: exceeds authority
   ├ ✓ fix-login         │     primary's note: "I don't know the house style; ask."
   └ ◉ api             1 │     Which heading level should the API page use?
     ├ ● write tests     │       1 h2 under Reference
     └ ◉ docs          ? │       2 h1, its own page
 ● herder                │
   └ ● p2d-1-design      │   ◉ approval · Bash  (herder › p2d-1-design)                 m2 · 6m
 ○ infra                 │     escalated: timeout (primary didn't answer in 5m)
                         │     $ git push --force-with-lease
 ─────────────────────── │
 attention      priority │
                         │
 ◉ api · app · box       │ ┃ answer docs: ▌
 ◉ docs · app · box      │ ╹▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀
 ✓ fix-login · app · box │ 1-9 pick · enter type · y/n allow/deny · l open session
 ● p2d-1 · herder · m2   │
 ● write tests · app     │
                         │
                         │
 ─────────────────────── │
 + new            ≡ menu │
─────────────────────────┴──────────────────────────────────────────────────────────────────────────
 NAVIGATE  j/k move  1-9 pick  y/n allow/deny  l open  esc back                  ● box ● m2 ◌ vault
```

```text
 inbox 2                     < back │ switch
 ! 2 need you                         │   !
────────────────────────────────────────────
 > ! question - docs              box 2m
   app > api > docs
   escalated: exceeds authority
   "I don't know the house style; ask."
   Which heading level for the API page?
     1 h2 under Reference
     2 h1, its own page

   ! approval - Bash               m2 6m
   herder > p2d-1-design
   escalated: timeout
   $ git push --force-with-lease

 1 h2   2 h1   enter type   l open   < back
```

### 2.4 Pull requests

The PR view lists every session's PRs grouped by project. `l` jumps to the owning
session. The `prs` tab of a session shows the same rows filtered to that session.

```text
 herder         inbox 2 «│ pull requests · 4 open                    by project · o open in browser
                         │
 projects                │ app
 ◉ app                 2 │   #12  open    ci ✓  review …  merge ✓  Add health endpoint       api
   ├ ✓ fix-login         │   #9   open    ci ✗  review ✗  merge ✗  Fix login redirect   fix-login
   └ ◉ api             1 │   #7   merged  ci ✓  review ✓           Bump axum            fix-login
     ├ ● write tests     │
     └ ◉ docs          ? │ herder
 ● herder                │ ▶ #88  draft   ci …  review –  merge ?  P2d.1 · Design spec     p2d-1
   └ ● p2d-1-design      │
 ○ infra                 │ infra
                         │   #3   closed  ci –  review –           Try k3s                (none)
 ─────────────────────── │
 attention      priority │
                         │
 ◉ api · app · box       │
 ◉ docs · app · box      │
 ✓ fix-login · app · box │
 ● p2d-1 · herder · m2   │
 ● write tests · app     │
                         │
                         │
 ─────────────────────── │
 + new            ≡ menu │
─────────────────────────┴──────────────────────────────────────────────────────────────────────────
 NAVIGATE  j/k move  o browser  l session  L link  x unlink  esc back            ● box ● m2 ◌ vault
```

```text
 prs 4                       < back │ switch
 ! 2 need you - 3 running             │   !
────────────────────────────────────────────
 app
   #12 open   ci v  rev .  Add health end.
   #9  open   ci x  rev x  Fix login redi.
   #7  merged ci v  rev v  Bump axum
 herder
 > #88 draft  ci .  rev -  P2d.1 - Design

 o browser  l session  L link  < back
```

### 2.5 Accounts

Accounts are listed per machine, with usage bars (`█░`, or `#-` in ASCII) and reset times.
Every account takes part in failover; the machine's pin is shown here but not editable from
the client: there is no command for it (§10).

```text
 herder         inbox 2 «│ accounts                                                n add · esc back
                         │
 projects                │ box                                          failover pin: claude
 ◉ app                 2 │ ▶ claude-main   claude                  3 sessions
   ├ ✓ fix-login         │     5h    ███████░░░░░░░░░░░░░ 38%   resets 14:20
   └ ◉ api             1 │     week  ██░░░░░░░░░░░░░░░░░░ 12%   resets Mon
     ├ ● write tests     │   claude-alt    claude                  0 sessions
     └ ◉ docs          ? │     5h    █░░░░░░░░░░░░░░░░░░░  4%   resets 15:05
 ● herder                │   codex-work    codex                   1 session
   └ ● p2d-1-design      │     day   ██████████████████░░ 91%   resets 23:00    ◉ near limit
 ○ infra                 │
                         │ m2
 ─────────────────────── │   claude-home   claude                  1 session
 attention      priority │     5h    ███░░░░░░░░░░░░░░░░░ 17%   resets 13:40
                         │
 ◉ api · app · box       │ failover only happens when a turn hits a limit: it rotates to the
 ◉ docs · app · box      │ provider's account with the most room left, same model. never early.
 ✓ fix-login · app · box │
 ● p2d-1 · herder · m2   │
 ● write tests · app     │
                         │
                         │
 ─────────────────────── │
 + new            ≡ menu │
─────────────────────────┴──────────────────────────────────────────────────────────────────────────
 NAVIGATE  j/k move  n add account  l log in again  esc back                     ● box ● m2 ◌ vault
```

```text
 accounts                    < back │ switch
 ! 2 need you - 3 running             │   !
────────────────────────────────────────────
 box                            pin: claude
 > claude-main  claude
     5h   #######------------- 38% 14:20
     week ##------------------ 12% Mon
   codex-work   codex
     day  ##################-- 91% 23:00
 m2
   claude-home  claude
     5h   ###----------------- 17% 13:40

 n add  l log in  < back
```

`l` on an account (owners only) logs it in again once its login expired: the provider's own
login runs in the account's config dir in a login terminal, as adding one does. The key and
its button show only while an account is selected.

### 2.6 Fleet

The fleet view is today's machines panel, extended with the vault's hosts and fork.
`F` on any session opens the fork dialog: enter forks it onto a host paired as owner, else
the dialog shows the command to run there (P0.13).

```text
 herder         inbox 2 «│ fleet · 3 machines               a add · e rename · d forget · F fork
                         │
 projects                │ ▶ ● box     owner   10.0.0.4:7447   cpu 23%  mem 41%  turns 3/6
 ◉ app                 2 │             SHA256:9f2c…41ab   accounts 3   sessions 5
   ├ ✓ fix-login         │   ● m2      member  m2.lan:7447     cpu 61%  mem 72%  turns 2/2 full
   └ ◉ api             1 │             SHA256:01de…77c0   accounts 1   sessions 2   no terminals
     ├ ● write tests     │   ◌ vault   owner   vault.lan:7447  read-only index
     └ ◉ docs          ? │     ● box      online
 ● herder                │     ● m2       online
   └ ● p2d-1-design      │     ✗ oldbox   offline · last seen 3h ago
 ○ infra                 │         ◉ api-refactor · app        F fork onto box or m2
                         │
 ─────────────────────── │
 attention      priority │
                         │
 ◉ api · app · box       │
 ◉ docs · app · box      │
 ✓ fix-login · app · box │
 ● p2d-1 · herder · m2   │
 ● write tests · app     │
                         │
                         │
 ─────────────────────── │
 + new            ≡ menu │
─────────────────────────┴──────────────────────────────────────────────────────────────────────────
 NAVIGATE  j/k move  a add  e rename  d forget  F fork  esc back              ● box ● m2 ◌ vault
```

### 2.7 Switch account / provider / model (picker)

This is OpenCode's model picker reshaped around herder's switches:

- Accounts are grouped as *same provider* (the conversation continues) or *other provider*
  (the transcript is replayed).
- The current account is marked `●`.
- Busiest-window usage is shown on each row.
- The model is free text, with client-side recent models listed.

```text
 herder         inbox 2 «│ [chat]  tasks 2  prs 1  term                    claude-main · opus · ask
                         │
 projects                │ ┃ Add a health endpoint and test it.
 ● app                 1 │
   ├ ✓ fix-login         │   + Th┌─ switch · api ───────────────────────────────────── esc ─┐
   └ ● api               │   → Re│ search: ▌                                                │
     ├ ● write tests     │   ✱ Gr│                                                          │
     └ ◉ docs          ? │       │ same provider · conversation continues                   │
 ● herder                │ ┃ # ca│ ● claude-main     5h 38%   current                       │
   └ ● p2d-1-design      │ ┃ runn│   claude-alt      5h  4%                                 │
 ○ infra                 │ ┃ test│                                                          │
                         │ ┃ … 9 │ other provider · replays the transcript                  │
 ─────────────────────── │       │   codex-work      day 91%                                │
 attention      priority │   ← Ed│                                                          │
                         │   ◇ Ta│ model: opus  (tab to edit · recent: sonnet, opus)        │
 ● api · app · box       │       │                                                          │
 ◉ docs · app · box      │   I ad│ enter switch · tab model · esc close                     │
 ✓ fix-login · app · box │   load└──────────────────────────────────────────────────────────┘
 ● p2d-1 · herder · m2   │
 ● write tests · app     │ ┃ write the docs page too, and link it from @README.md▌
                         │ ┃
                         │ ┃ claude-main · opus · ask
 ─────────────────────── │ ╹▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀
 + new            ≡ menu │ ⠹ working · 1m 12s  ctrl+c stop                       claude-main 5h 38%
─────────────────────────┴──────────────────────────────────────────────────────────────────────────
 PROMPT  enter send  shift+enter newline  / commands  @ mention                  ● box ● m2 ◌ vault
```

### 2.8 Other dialogs

All other dialogs use the same frame: a centred box with a bold title, `esc` at the top
right, a search box when there is a list, and footer hints.

| dialog | width | contents |
|---|---|---|
| new session | 60 | machine, repo, account, model, mode (today's fields, as a form) |
| add machine | 60 | link or host / fingerprint / code; the fingerprint must be confirmed |
| add account | 60 | provider, id, label, config dir, then a login terminal |
| terminals | 60 | `+ new terminal`, then open shells (owner only) |
| fork | 60 | where the session runs, hosts paired as owner; enter forks onto one, else `herder fork <id>` to run there |
| command palette | 88 | every command and its key; "Suggested" first (OpenCode `ctrl+p`) |
| help | 88 | the keymap from §4, grouped and filterable with `/` |
| theme | 60 | built-in and user themes with live preview |
| confirm | 44 | destructive actions (archive with force, forget machine, unlink PR) |

---

## 3. Information architecture

### 3.1 Concepts mapped onto Herdr's model

| Herdr | herder | where it shows |
|---|---|---|
| session (server) | the set of paired **machines** | bottom-bar connection marks; fleet view |
| workspace / space | **project** (`github.com/org/repo` or `HOST:/path`) | sidebar *projects* root rows |
| worktree child workspace | **session** (one worktree/branch on one host) | indented under its project with `├`/`└` |
| — | **task child** (spawned through herder-tasktools) | indented under its parent session; folds with `z` |
| tab | **session tabs**: chat, tasks, prs, term | top row of the main pane |
| pane | the chat transcript, or an attached **terminal** | main pane; a terminal attaches full-screen |
| agent state | `SessionStatus` plus "done" (client-side) | the glyph on every row |
| agents panel | **attention** list | lower sidebar section |
| `machine` token | machine label on rows | right-aligned on sidebar rows, or after `·` |
| goto picker (`prefix+g`) | **go to** | dialog; full-screen switcher on a phone |
| menu | **views** and **menu** | sidebar footer `≡ menu`; switcher sections |

The global **views** (inbox, PRs, accounts, fleet) have no Herdr equivalent. They render
in the main pane, with the sidebar staying put. `esc` returns to the last session.

Grouping (`v`) switches the projects tree to **machine ▸ sessions**. For a vault this is
**host ▸ sessions**, with offline hosts marked `✗` and "last seen". This is today's
behaviour, kept.

### 3.2 States and roll-up

| state | source | Unicode | ASCII | colour token |
|---|---|---|---|---|
| needs you | `NeedsYou`, or `children_need_you > 0` | `◉` | `!` | `attention` |
| error | `Error` (the last turn failed) | `✗` | `x` | `error` |
| done | turn completed and not opened since (client) | `✓` | `v` | `info` |
| running | `Running` | `●` | `*` | `warning` |
| waiting | `WaitingForCapacity` | `◌` | `~` | `secondary` |
| idle | `Idle`, already seen | `○` | `o` | `textMuted` |
| archived | `Archived` | `▪` | `_` | `textMuted` |
| moved | `Moved` (taken over elsewhere) | `→` | `>` | `secondary` |
| unknown | `Unknown`, or not loaded | `·` | `.` | `textMuted` |

The waiting kind refines "needs you" in the text but not in the glyph: `approve?` or
`question` on wide rows, and a right-aligned `?` (question) or `!` (approval) on narrow
rows, as today.

Roll-up rules:

- **Order.** The priority is
  `needs you 6 > error 5 > done 4 > running 3 > waiting 2 > idle 1 > archived/moved/unknown 0`.
- **Where it applies.** A folded session takes the maximum over its task subtree. A project
  takes the maximum over its sessions; a machine (in machine grouping), over its sessions.
  The phone header summarises all of them.
- **Error sits above done** (a change from Herdr). A failed turn has to be seen.
  `LimitReached` without failover is an error.
- **Done.** A session becomes *done* when a client watching it sees a `Running → Idle`
  transition. Opening it marks it seen. Seen state is per client and lives in memory, plus
  `cache/` so it survives a restart. This needs no protocol change.
- **No animation in the sidebar** (Herdr). Motion lives only in the chat status line.

---

## 4. Keymap

The rule from today's TUI stays: **every action has a key a phone keyboard has** (a letter,
a digit, Enter or ⌫). `Esc`, `Tab`, arrows and `ctrl` chords are alternatives, never the only
way.

### 4.1 Modes

| mode | badge | entered by | left by |
|---|---|---|---|
| PROMPT | `PROMPT` | opening a session, `i`, `Enter` on chat | `esc`, `⌫` on empty, `tab` |
| NAVIGATE | `NAVIGATE` | the above, or focus on the sidebar or a view | `i`, `Enter` (open or write) |
| LEADER | `LEADER` | `ctrl+x` in any mode | the next key, `esc`, or 2 s |
| APPROVAL / QUESTION | `APPROVAL` | a pending request in the open session | answering it, `esc` → NAVIGATE |
| DIALOG | — | any dialog | `esc`, `⌫` on empty search |
| TERMINAL | — | attaching a terminal (full screen) | `ctrl+] d` (today's) |

The bottom bar always shows the mode badge and the keys that work right now, as Herdr's
mode bar does. Unlike Herdr, it has its own row and never covers content.

### 4.2 Global (leader, or bare in NAVIGATE)

`ctrl+x k` ≡ `k` in NAVIGATE.

| key | action | | key | action |
|---|---|---|---|---|
| `?` | help | | `n` | new session |
| `/` | go to (search) | | `s` | switch account / provider / model |
| `:` | command palette (also `ctrl+p`) | | `t` | terminals (owner only) |
| `I` | inbox (`i` from the sidebar, as today) | | `F` | fork onto a host |
| `P` | all PRs | | `p` | the session's prs tab |
| `A` | accounts | | `L` | link a PR |
| `m` | fleet (machines) | | `a` | add machine |
| `v` | group by project / machine | | `z` | fold or unfold a task subtree |
| `b`* | toggle sidebar (`ctrl+x b` only) | | `d`* | toggle details panel (`ctrl+x d` only) |
| `T` | theme picker | | `r` | reconnect |
| `E` | rename the session (`r` is reconnect) | | `R` | ask AI to title the session again |
| `1`–`9` | jump to the Nth attention row (leader only; bare digits answer) | | `q` | quit (`ctrl+c` twice) |

\* These two exist only under the leader, because bare `b` already scrolls up a page.

### 4.3 NAVIGATE — moving around

| key | sidebar / list views | chat |
|---|---|---|
| `j`/`k`, `↓`/`↑` | move the cursor (preview, no switch) | scroll a line |
| `Enter`, `l`, `→` | open | focus the prompt (`Enter`, `i`) |
| `h`, `⌫`, `←`, `esc` | back / up a level | to the sidebar |
| `tab` / `shift+tab` | cycle sidebar → main → details | same |
| `g`/`G`, `Home`/`End` | first / last | top / bottom (`G` follows) |
| `space`/`b`, `PgDn`/`PgUp`, `ctrl+d`/`ctrl+u` | page | page |
| `[` / `]` | — | previous / next message or tool item (item cursor) |
| `e` | — | expand or collapse the item under the cursor (Enter on the item does the same) |
| `c` | — | copy the item under the cursor (OSC 52) |
| `o` | open the PR in the browser | open the child session of a task item |
| `u` | — | go to the parent session (task tree; OpenCode `session_parent`) |
| `,` / `.` | — | previous / next sibling task (OpenCode `child_cycle`) |
| `x` | unlink PR (PR lists) | stop the running turn (also `ctrl+c`) |

The **item cursor** (`[`/`]`) fixes OpenCode's mouse-only "click to expand": every
foldable thing can be reached from the keyboard.

### 4.4 PROMPT (editor; names follow OpenCode's `input_*`)

| key | action |
|---|---|
| `Enter` | send (queued if a turn is running, marked `QUEUED`) |
| `shift+Enter`, `alt+Enter`, `ctrl+j` | newline |
| `↑` / `↓` on the first / last line | prompt history (per session, then global) |
| `/` at column 0 | command autocomplete (§5.2) |
| `@` | mention autocomplete (§5.2) |
| `tab` | accept completion; with no popup, move focus |
| `esc` | close the popup, otherwise go to NAVIGATE |
| `ctrl+c` | clear the input; on empty, stop the turn; idle and empty, twice to quit |
| `ctrl+x e` | edit the prompt in `$EDITOR` |
| `ctrl+a`/`ctrl+e`, `alt+b`/`alt+f`, `ctrl+w`, `ctrl+u`, `ctrl+k` | readline editing |
| paste ≥ 3 lines or > 150 chars | collapses to `[pasted ~N lines]`; sent in full |

### 4.5 APPROVAL / QUESTION

| key | action |
|---|---|
| `y` / `n` | allow / deny |
| `←`/`→`, `h`/`l` then `Enter` | choose a button, confirm |
| `1`–`9` | pick a choice |
| `Enter` on a free-text question | type the answer in the prompt |
| `f` | full-screen the request (long commands and diffs; OpenCode `ctrl+f`) |
| `esc` | leave it pending and go to NAVIGATE (it stays in the inbox) |

### 4.6 Mouse and touch

This is Herdr's model on top of today's touch support (P2.14).

- **Tap or click a row:** selects it; tapping the selected row opens it. Like Herdr, the
  action fires on release.
- **Tap a tab, button, `« »`, `▼ ▶`, `≡ menu`, `+ new` or `switch`:** does what it shows.
- **Tap a tool line, the reasoning line or `… N more lines`:** expands it (OpenCode).
- **Wheel or swipe:** scrolls 3 lines in chat and 1 row in lists. Over the tab row it
  switches tabs.
- **Drag** the sidebar's `│` to resize it (18–36 columns, default 26, saved). Double-click
  resets it. There is no drag on a phone.
- **Shift-drag** selects text. `:mouse off` hands the mouse back to the terminal (today's).
- **No motion reporting.** Phones aren't flooded (P2.14).

### 4.7 Phone keys (Termius and other SSH apps; P2.16)

This section keeps P2.16 (#87) as it is. Termius sends gestures as keys, never as mouse
events, and the README tells users to map them like this:

| gesture | key | does (≤ 64 columns) |
|---|---|---|
| swipe ↑ / ↓ | `↑` / `↓` | move the cursor, or scroll |
| two-finger swipe ↑ / ↓ | `PgUp` / `PgDn` | page |
| swipe ← / → | `shift+tab` / `tab` | move the focus along the button bar (it wraps and scrolls) |
| tap `⏎` | `Enter` | press the focused button, otherwise open or send |
| `esc` / `⌫` | `esc` / `⌫` | back, close a dialog, or PROMPT → NAVIGATE (`⌫` on empty) |

Rules for the new design:

- **On a phone, `tab` always drives the button bar.** This covers every mode, so approving
  stays `tab`, `Enter`. The exceptions are forms whose fields `tab` moves between, and an
  open `/` or `@` popup, where `tab` completes. Region cycling (§4.3) is a wide-screen
  behaviour only; a phone shows one region at a time anyway.
- **The button bar is the phone's mode bar.** Both come from one hint list, so a key shown
  on the desktop is a button on the phone. That includes P2.16's inbox, PRs, accounts and
  machines buttons.
- **The `switch` button is the first stop for `shift+tab`** from an unfocused bar.
- **Moshi uses mouse mode** and gets §4.6.

### 4.8 Config

Keybinds live in `tui.json` under `keybinds`, using OpenCode's names where they exist
(`session_new`, `session_interrupt`, `model_list`, `sidebar_toggle`, `input_newline`,
`messages_page_up`, …). Herder-only actions get new names (`inbox`, `prs_all`, `accounts`,
`fleet`, `fork`, `terminal_list`, `approval_allow`, `approval_deny`). Values are
comma-separated alternatives; `"none"` unbinds. `leader` is set separately.

---

## 5. Agent interaction (from OpenCode)

### 5.1 Transcript items

Each `ItemBody` and event renders as follows.

| item | renders as |
|---|---|
| `UserMessage` | `┃` left bar in `primary`; body on `backgroundPanel`; `QUEUED` badge while queued |
| `AssistantMessage` | markdown, indented 3, no frame; the reply ends with `▣ account · model · 1m 12s` (and `· interrupted`) |
| `Reasoning` | one line, `+ Thought: <first line> · 4s` in `warning` (muted); `e` or a click expands; hidden with `/thinking` |
| `ToolCall` pending | `~ <tool>…` muted |
| `ToolCall` + result, short | inline: glyph + one-line summary (table below) |
| `ToolCall` + result with output | block: `┃` bar on `backgroundPanel`, `# title`, 10 lines then `… N more lines` |
| `ToolResult` `is_error` | inline line in `error`; expands to the error text |
| tool awaiting approval | its inline line turns `warning` until answered; struck through if denied |
| `ChildSpawned` | `◇ Task <title> → <branch>`, then a live `↳ <child's current tool>` or `↳ N tool calls · 1m 02s`; `o` opens it |
| `ChildReported` | `↳ report:` plus the first line of the report, under the task line |
| approval / question resolved | one muted line: `△ allowed Bash · by you` / `? answered by primary` |
| `ModelSwitched`, `AccountSwitched`, `ProviderSwitched`, `PermissionModeChanged`, `SessionForked` | centred rule: `── switched to codex-work (transcript replayed) ──`; a fork: `── switched to devbox (from laptop) ──` |
| `TurnFailed` | `┃` bar in `error` with the class (limit / auth / transient / fatal) and the failover outcome |
| `PrLinked` / `PrUpdated` | `⎇ #12 opened · ci …` one line; updates edit that line rather than adding new ones |
| live text deltas | the in-progress item with a `▌` cursor (today's) |

Tool glyphs (Unicode / ASCII) are picked by tool name. Unknown tools fall back to `⚙`.

| tool | glyph | inline summary | block when |
|---|---|---|---|
| Bash / shell | `$` / `$` | `$ cmd` | output exists |
| Read | `→` / `>` | `→ Read path` | never |
| Write | `←` / `<` | `← Write path` | expanded: the content, numbered |
| Edit / patch | `←` / `<` | `← Edit path +12 −1` | always: diff, split at ≥ 120 columns, else unified |
| Grep / Glob | `✱` / `*` | `✱ Grep "pat" in dir (N matches)` | never |
| Web fetch / search | `◈` / `@` | `◈ Fetch url` | never |
| Todo | `☐` / `[]` | `☐ Todos 2/5` | expanded: `[✓] [•] [ ]` |
| herder-tasktools | `◇` / `+` | `◇ spawn` / `send` / `wait_for` … | as `ChildSpawned` |
| other / MCP | `⚙` / `>` | `⚙ name k=v` | output exists, 3 lines |

Tool names come from the vendor CLI through the adapter (`ToolCall{name,input}`). The
table matches on the provider's names (Claude `Bash`, `Edit`, …; Codex `shell`,
`apply_patch`, …). A tool that isn't recognised still renders generically.

### 5.2 Prompt: `/` commands and `@` mentions

`/` at column 0 opens the popup (fuzzy match, at most 10 rows, above the prompt). `//`
sends a literal `/`. The commands replace today's `:` palette words, which still work after
`:`.

| command | does |
|---|---|
| `/new` | new session dialog |
| `/model <name>` | `SetModel` |
| `/mode read_only\|ask\|auto_edit\|full_access` | `SetPermissionMode` |
| `/switch` | switch dialog (account / provider / model) |
| `/stop` | `Interrupt` |
| `/archive[!]` | archive (`!` = force) |
| `/pr <n\|url>`, `/unpr` | link / unlink a PR |
| `/term` | terminals (owner) |
| `/down [project]` | compose down (owner) |
| `/fork` | fork dialog |
| `/thinking`, `/details` | show or hide reasoning / tool output |
| `/theme`, `/glyphs ascii\|unicode`, `/mouse on\|off` | display settings, saved |
| `/inbox`, `/prs`, `/accounts`, `/fleet` | open a view |
| `/help`, `/quit` | |

`@` opens mention completion:

- `@<child>`: the session's task children, inserted as their branch name.
- `@<path>`: files in the session's worktree. This needs a file-list call that client-core
  doesn't have (§10). Until it exists, `@` offers children only.

### 5.3 Approvals and questions

These follow OpenCode's permission panel, inline, replacing the prompt; the mockups are in
§2.1.

- **Header:** `△ approval · <tool>` (or `? question from <task>`), with the age on the
  right.
- **Body:** the command or diff, capped at 15 rows. `f` shows it full-screen.
- **Routed requests:** for an `ApprovalEscalated` or `QuestionEscalated` request, the body
  also shows *why* it was escalated (`marked by primary` / `exceeds authority` / `timeout`)
  and the primary's note.
- **Buttons:** `[ allow ]   deny`, or the choices `1…N` plus "or type an answer".
- **Members can answer.** They drive sessions. Only terminals are owner-only.
- **Waiting requests show elsewhere too.** While any request waits, the bottom bar shows
  `△ N waiting · I inbox`. A request for a session that isn't open also raises a toast.

### 5.4 Toasts

Toasts are OpenCode's, one at a time:

- Position: top right, 2 cells in.
- Width: `min(60, width-6)`.
- Variants: info, success, warning, error, each with `┃` side bars.
- Duration: 5 s; errors stay 10 s.

On a phone a toast is a one-line banner above the button bar (Herdr). Toasts fire for:

- a request routed to you
- a session finishing while you're elsewhere
- a turn failing
- failover happening
- a machine disconnecting

They are **on by default**, unlike Herdr's. Toasts are suppressed for the session you are
looking at.

---

## 6. Theme and glyphs

### 6.1 Tokens

These are OpenCode's names, so its theme files are easy to port, plus herder's state
tokens.

- **Core:** `primary secondary accent error warning success info text textMuted
  selectedListItemText background backgroundPanel backgroundElement backgroundMenu border
  borderActive borderSubtle`
- **Diff:** `diffAdded diffRemoved diffContext diffHunkHeader diffHighlightAdded
  diffHighlightRemoved diffAddedBg diffRemovedBg diffContextBg diffLineNumber
  diffAddedLineNumberBg diffRemovedLineNumberBg`
- **Markdown:** `markdownText markdownHeading markdownLink markdownLinkText markdownCode
  markdownBlockQuote markdownEmph markdownStrong markdownHorizontalRule markdownListItem
  markdownListEnumeration markdownCodeBlock`
- **Syntax:** `syntaxComment syntaxKeyword syntaxFunction syntaxVariable syntaxString
  syntaxNumber syntaxType syntaxOperator syntaxPunctuation`
- **herder:**
  - `attention` (needs you)
  - `stateRunning`, `stateDone`, `stateWaiting`, `stateIdle`, `stateError`. These default
    to `warning`, `info`, `secondary`, `textMuted` and `error`.
  - `usageLow`, `usageHigh` (≥ 70%) and `usageFull` (≥ 90%).
  - `prOpen`, `prMerged`, `prDraft`, `prClosed`.

**Format.** This is OpenCode's: `{ "defs": {name: "#hex"}, "theme": {token: "#hex" | defName |
otherToken | ansiIndex | "none" | {"dark": …, "light": …}} }`. User themes go in
`<config_dir>/themes/<name>.json`, where `<config_dir>` is `$XDG_CONFIG_HOME/herder`.
`tui.json` stores `"theme"` and `"mode": "dark" | "light" | "auto"`.

### 6.2 Default palettes (`herder`)

| token | dark | light |
|---|---|---|
| primary | `#7aa2f7` | `#2f5bb7` |
| secondary | `#6c8ebf` | `#4a6fa5` |
| accent | `#bb9af7` | `#7847bd` |
| attention | `#e879c6` | `#b0307f` |
| error | `#f7768e` | `#c4334b` |
| warning | `#e0af68` | `#a86b00` |
| success | `#9ece6a` | `#3f7d1f` |
| info | `#7dcfff` | `#00739e` |
| text | `#e6e6e6` | `#1c1c1c` |
| textMuted | `#7f8492` | `#6b6f7a` |
| background | `#111216` | `#ffffff` |
| backgroundPanel | `#181a20` | `#f6f7f9` |
| backgroundElement | `#21242c` | `#eceef2` |
| border | `#3b3f4a` | `#c6cad3` |
| borderActive | `#7aa2f7` | `#2f5bb7` |
| borderSubtle | `#2a2d36` | `#dfe2e8` |
| diffAddedBg / diffRemovedBg | `#1d2f24` / `#3a1f26` | `#dcf2e1` / `#f8dde1` |

The full set, including markdown and syntax, is filled in by P2d.2 from these anchors.
Contrast gets checked at ≥ 4.5:1 for `text` and ≥ 3:1 for state colours on `background`
and `backgroundPanel`.

**`ansi` theme.** Every token maps to one of the 16 named colours, matching today's
choices:

| tokens | colour |
|---|---|
| primary, info | cyan |
| attention | magenta |
| error | red |
| warning | yellow |
| success | green |
| secondary | blue |
| textMuted | dark gray |

Backgrounds are `none`. It is used automatically when `COLORTERM` isn't
`truecolor`/`24bit`, or when chosen.

### 6.3 Glyph sets

The `/glyphs` setting picks the set. If it is unset, the choice is by width: **ASCII at
≤ 64 columns, Unicode above**, as in P2.15.

Instead of today's post-render fold, each set becomes an explicit table that widgets ask
for. That stops collisions (today `◌` and `○` both fold to `o`). The fold stays only as a
safety net for transcript text.

| meaning | Unicode | ASCII |
|---|---|---|
| states | `◉ ✗ ✓ ● ◌ ○ ▪ → ·` | `! x v * ~ o _ > .` |
| tree | `├ └ │` and fold `▾ ▸` | `├ └ │` (box drawing is kept) and fold `- +` |
| bars | `┃` user/tool, `╹▀` prompt cap | `│`, `─` |
| cursor / selection | `▌`, `▶` | `▌`→`\|`, `>` |
| expand | `▼ ▶`, `…` | `v >`, `...` |
| usage | `█░` | `#-` |
| CI / review / merge | `✓ ✗ … –` | `v x . -` |
| connection | `● ◌ ✗` | `* ~ x` |
| spinner | `⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏` (80 ms) | `\| / - \` (120 ms) |
| tools | `$ → ← ✱ ◈ ☐ ◇ ⚙` | `$ > < * @ [] + >` |
| header / back | `‹ back`, `≡ menu`, `«` / `»` | `< back`, `= menu`, `<<` / `>>` |

Every glyph is single-width and outside the East Asian ambiguous ranges that break phone
fonts. P2.15's cursor anchoring stays.

---

## 7. Layout rules

| width | layout |
|---|---|
| ≤ 64 | phone: 2-row header, one full-width pane, switcher, button bar, ASCII |
| 65–119 | sidebar (default 26, 18–36) + main; details as an overlay (`ctrl+x d`) |
| ≥ 120 | sidebar + main + details (42); split diffs |

Other rules:

- **Sidebar collapse.** `ctrl+x b` collapses the sidebar to a 4-column strip of state
  glyphs (projects, then attention), as in Herdr's compact mode. Tapping `»` expands it
  again.
- **What is saved.** Widths, the collapsed state and grouping are saved in `tui.json`.
- **Height.** Under 20 rows the prompt's meta line and the main-pane tab row are hidden.
  Tabs remain reachable through `p` and `t`.

---

## 8. Component inventory (for P2d.2)

Each component is a ratatui widget with a snapshot test at 45, 100 and 160 columns, in both
glyph sets where they differ.

| component | used by | notes |
|---|---|---|
| `Frame` | all | breakpoints, regions, focus ring, mode |
| `Sidebar` (`ProjectTree`, `AttentionList`, footer) | frame | roll-up, fold, grouping, collapse, resize hit-targets |
| `StateGlyph` + `rollup()` | everywhere | §3.2 table; pure function, unit-tested |
| `SeenTracker` | sidebar, toasts | client-side "done", persisted in cache |
| `TabBar` | main | chat / tasks / prs / term, counts, tap targets |
| `ModeBar` | frame | badge, contextual hints, connection marks |
| `MobileHeader` + `Switcher` | phone | Herdr's mobile model |
| `ButtonBar` | phone | today's, fed by the same hint list as `ModeBar` |
| `Transcript` | chat | item list, item cursor, follow mode, lazy markdown |
| `MessageItem`, `ReasoningItem` | transcript | §5.1 |
| `ToolInline`, `ToolBlock`, `DiffView` | transcript | §5.1 tool table; split/unified |
| `TaskItem` | transcript, tasks tab | live `↳` line, open child |
| `Prompt` (editor, history, paste collapse, `$EDITOR`) | chat | §4.4 |
| `Autocomplete` | prompt | `/` and `@` sources |
| `RequestPanel` (approval, question) | chat, inbox | §5.3 |
| `DetailsPanel` (sections) | ≥ 120 / overlay | ordered sections, fold |
| `ListView` (cursor, group headers, search) | inbox, PRs, accounts, fleet, dialogs | one list widget for all |
| `Dialog` + `SelectDialog` + `FormDialog` | §2.8 | sizes 44/60/88, backdrop, esc |
| `Toast` | frame | §5.4 |
| `UsageBar` | accounts, details, switch | `█░` / `#-` |
| `Theme` (+ loader) | everywhere | §6; `ansi` auto-fallback |
| `GlyphSet` | everywhere | §6.3; replaces the frame fold |
| `Keymap` (+ config) | input | named actions, leader, modes, `tui.json` |

Suggested order: Frame/Sidebar/StateGlyph → Transcript and tools → Prompt and
RequestPanel → views → theme and keybind config.

---

## 9. Migration (what changes for today's users)

| today | after P2d |
|---|---|
| list on the left, `Enter` opens, `i` to write | same; opening a session lands in the prompt, and `esc` gets you back to keys |
| `:` palette words (`:model`, `:mode`, `:archive`, `:down`, `:mouse`, `:glyphs`, …) | `/` commands in the prompt; `:` still opens the palette and accepts the old words |
| `I`, `P`, `A`, `m`, `s`, `t`, `R`, `p`, `L`, `z`, `v`, `y`/`n`, `1-9`, `g`/`G` | unchanged in NAVIGATE; also under `ctrl+x` |
| PR strip and resources strip above the transcript | `prs` tab, and the details panel (≥ 120) or `ctrl+x d` |
| "needs you" etc. as words in the list | glyph + colour; words remain in the details panel and the phone header |
| hard-coded ANSI colours | `herder` theme, or `ansi`, which looks like today |
| ASCII applied as a whole-frame fold | explicit glyph tables; `/glyphs` and `tui.json` unchanged |
| header `‹ back · title · inbox N · +` and button bar on narrow screens | phone header with `switch`; button bar and its Tab/Enter focus (P2.16) kept |
| `Ctrl-] d` to detach a terminal | unchanged |

`tui.json` keeps `mouse` and `glyphs` and gains `theme`, `mode`, `leader`, `keybinds` and
`layout`. Nothing is migrated: absent keys take defaults.

---

## 10. Gaps that need a `[CONTRACT]` todo

None of these blocks P2d.2; each feature below ships without the gap until it is filled.

1. **Worktree file list** for `@path` mentions: a client-core call to list or search files
   in a session's worktree.
2. **Toggle failover / pin from the client.** There is no `CommandBody` for it today, so
   the accounts view is read-only for failover.
3. **Token / context usage per session.** OpenCode shows `41k (21%) · $0.12`. herder has
   account usage windows only, so the status line shows those. A per-turn usage field
   would be a protocol change.

---

## 11. What we deliberately don't copy

**From Herdr:**

- **Panes and splits, and embedded terminals in panes.** herder's unit is a session, not a
  PTY. Terminals stay owner-only and attach full-screen. Embedding a VT emulator is a
  separate decision.
- **Same glyph, different colour, for states.** Ours always differ in shape, because of
  phones, colour blindness and the `ansi` theme.
- **The mode bar covering the last content row.** Ours has its own row.
- **Toasts off by default.** Ours are on.
- **Workspace drag-reordering.** Projects sort by attention, then name.
- **Detecting agent state by scraping the screen.** herder's daemon *knows* the state from
  its event log.

**From OpenCode:**

- **"Allow always" approvals.** herder approvals are allow/deny per request. Standing rules
  belong to permission modes.
- **Shell mode (`!`) in the prompt.** A shell is a terminal, owner-only; it is never part
  of a member's prompt.
- **Session share / unshare, fork, undo / redo, compact, timeline.** These have no herder
  backing and aren't in scope.
- **Cost display in dollars.** herder doesn't see billing.
- **Plugin slots and the which-key panel.** These are speculative for us; the help dialog
  and mode bar cover discoverability.
- **Key overloading** (`ctrl+f` for four things, `ctrl+d` for delete *and* exit). Each herder
  key has one meaning per mode.
- **Agent cycling with `tab`.** herder has no agent profiles. `tab` moves focus.
