# herder Mac and iOS design

Status: the spec for the Apple apps (P7.20), written against the apps as they are on `main`
after P7.19. One SwiftUI codebase, `apple/HerderKit`, runs on the Mac, the iPad and the iPhone.
This spec says how each screen lays out per size class, so the phone stops drifting from the
Mac. It keeps the visual style: the dark, T3-like palette in `Design/Theme.swift` stays.

[`docs/tui-design.md`](tui-design.md) is the TUI's spec. Where the two overlap they use the
same concepts and names: machines, projects, sessions, children, needs you, the state glyph set,
approvals and questions, PRs, terminals. Where the apps differ from the TUI, this spec says so.

Every rule names the HerderKit type that implements it. A rule the code does not follow yet is
listed in §10 with its follow-up todo.

**Reading guide (about 10 minutes):** §1 for the decisions, §3 for the layouts, and the
screenshots in [`docs/screenshots/p7-20`](screenshots/p7-20).

---

## 1. Decisions at a glance

1. **One information architecture everywhere.** Board, Projects, Usage, Skills,
   Machines (with a vault inside), a session, and settings sheets. The Mac, the iPad and the iPhone show the
   same sections with the same names; only the navigation chrome changes (§3).
2. **Three layouts, chosen by width, not by device.** `FleetView` picks the layout from the
   horizontal size class; `ListAndSession` picks list-beside-session from the pane's width.
   - **Regular** (Mac window, iPad full screen or wide split): sidebar, list pane, session.
   - **Narrow regular** (a Mac window or iPad pane under 820 pt of content): sidebar, then the
     list *or* the session with a Back button.
   - **Compact** (iPhone, iPad slide-over and narrow split): a tab bar and navigation stacks;
     a session is pushed full screen.
3. **The session view is the same view everywhere** (`SessionView`): header, transcript, pinned
   request, composer. Only its secondary panes move: beside the chat on regular width, in a
   sheet on compact width (§6).
4. **Nothing is ever wider than the screen.** No fixed width over 320 pt on iOS outside a
   regular-width branch; content columns cap at 760 pt and centre (§5.2).
5. **Lists put live work first.** Needs you, then running, then idle; archived sessions go in a
   collapsed **Archived** group at the end; children sit under their parent (§4.7).
6. **State is a glyph plus a colour, never colour alone** (`StatusGlyph`), the same set on every
   row, header and card, as in the TUI.
7. **Touch is first-class on iOS.** 44 pt hit areas on iOS and iPadOS; the Mac keeps its
   denser 28–34 pt controls (§7).
8. **Dark only for now.** `HerderScene` forces `.dark`; the light e-ink palette in `Theme`
   stays defined for later.

---

## 2. Information architecture

### 2.1 Sections and their TUI names

| app section | what it holds | TUI equivalent | component |
|---|---|---|---|
| **Board** | every open task tree across all machines, by day, cleared by archiving | board | `BoardView` |
| **Projects** | every project with its sessions, children under parents | sidebar *projects* tree | `ProjectsView` (iPhone), `ProjectSessions` (Mac/iPad) |
| **Usage** | tokens and API-equivalent dollars over a period, per account and model | — | `UsageView` |
| **Skills** | the skill library on every machine, and a session's project skills | — | `SkillsView` |
| **Machines** | each paired machine: connection, load, accounts and usage | fleet + accounts | `MachinesView`, `MachineCard` |
| Vault | a vault's hosts and their sessions | fleet, host grouping | `VaultView` |
| **Session** | one session: transcript, request, composer | session pane, chat tab | `SessionView` |
| Terminal | owner-only shells in the session's worktree | term tab | `TerminalPane` |
| Inspector | the session's events and stats | details panel | `SessionInspector` |
| Settings | per machine and per project, as sheets | dialogs | `MachineSettingsSheet`, `ProjectSettingsSheet` |
| New session | pick a project, then an empty chat whose first message creates it | `n` dialog | `ProjectPicker`, `DraftSessionView` |

There is no app-wide settings screen: herder's settings belong to a machine or a project, so
they open from that machine's or project's row (`SidebarRow`'s gear, `MachineCard`'s gear,
the project header's gear).

### 2.2 Names in the UI

- Sections are written in title case in the chrome ("Add Account", "New Session") and as
  `SectionHeading` capitals inside panes ("NEEDS YOU 2", "ACTIVE", "RECENT", "ARCHIVED").
- The product is always lowercase `herder`.
- A session is named by its title; its branch shows in monospace (`Theme.monoSmall`) as a
  secondary line, never instead of the title.
- A child session is "Agent of ‹parent›" (`ChildBanner`), as the TUI's task child.

---

## 3. Size classes and navigation

| | Mac window / iPad regular | narrow regular (< 820 pt content) | iPhone / compact |
|---|---|---|---|
| frame | `DesktopShell` | `DesktopShell` | `FleetView.tabs` |
| navigation | sidebar: rail (76 pt) or full (228 pt) | same | tab bar: Board · Projects · Usage · Machines |
| section switch | sidebar rows; the pane title's menu (`Switcher`) | same | tabs |
| list | 380 pt pane beside the session (`ListAndSession`) | full width until a session opens | full screen, `NavigationStack` |
| session | fills the rest | replaces the list, with **Back** | pushed; tab bar hidden |
| inspector | 300 pt side pane in the session | side pane | sheet (§6) |
| new session | `ProjectPicker` sheet, draft in the session pane | same | sheet, then the draft full screen (`fullScreenCover`) |
| sheets | centred, 560 pt wide (`SheetScaffold`) | same | system sheet, full width |

Rules:

- **The size class decides, not the platform.** `FleetView` shows `tabs` when the horizontal
  size class is compact and `DesktopShell` otherwise, so an iPad in slide-over gets the iPhone
  layout. Code that differs per layout reads `horizontalSizeClass`, not `#if os(iOS)`, unless
  the difference is a platform API (AppKit text view, hover, keyboard shortcuts).
- **The sidebar** (`Sidebar`, `SidebarRail`) is grouped: what the user works on (Board, Chats), then **Workspace**, the setup (Providers, Skills, Machines, Vault), then the
  projects, then one footer line for the machines (`MachinesFooter`). It starts collapsed to
  the rail (`sidebarCollapsed`), which leaves the width to the session and lists the same
  things in the same order; ⇧⌘\ toggles it. The window's traffic lights sit in its top bar
  (`Sidebar.topBar`, 52 pt on the Mac).
- **A row shows only live state**: an amber count of what waits for the user, a blue count
  of what is running, nothing when quiet (`SidebarRow`). A project's state glyph, idle or
  done, is not shown there.
- **Projects are in the user's order** (`ProjectOrder`). Dragging a project onto another puts
  it in that one's place and switches the sort to Manual, the default; the ⇅ menu also sorts by
  Recent activity, Needs you first (waiting count, then running count, then recency) or Name.
  The sort and the manual order are saved per device (`projectSort`, `projectOrder`), and the
  pane title's `Switcher` lists projects in the same order. Projects with no activity for 30
  days (`ProjectGroup.quiet`) fold under "N more", unless one waits for the user or is open.
  The sessions no project holds yet stay last.
- **The machines footer** is a dot per machine coloured by its connection (`ConnectionMark`)
  and a summary ("4 machines · 1 offline"). Clicking it opens a popover with each machine's
  status, each row opening its page, and a link to Machines. On the rail it is the dots alone.
- **The list pane** is 380 pt (`ListAndSession`). ⌘\ hides it to give the session the whole
  width (`listHidden`); with no session open it always shows.
- **The pane header** (`Pane`) is a `PageHeader` (§8.4): the title (a menu of sections and
  projects, `Switcher`), the page's live status, the pane's actions on the right (`PaneButton`,
  `IconButton`), then a search field where the pane lists sessions.
- **The tab bar** on compact width has the same sections as the sidebar: Board (badged with the
  request count), Projects, Usage, Machines. Skills and a vault show inside Machines. Each tab is its own `NavigationStack` (`boardPath`,
  `projectsPath`), so switching tabs keeps each tab's place.
- **Back always returns to where the user came from**: the stack's back button on iPhone,
  `ListAndSession`'s Back on narrow regular, `ChildBanner`'s Back (⌘[) from a child to its
  parent.

Screenshots: `mac-home.png` (rail), `mac-sidebar-expanded.png`, `mac-sidebar-machines.png`
(the machines popover), `mac-narrow.png`, `ios-home.png`.

---

## 4. Lists and sections

### 4.1 Board

The app opens on the Board (§4.8): it is the home screen.

### 4.2 Projects and Chats (`ProjectsView`, `ProjectSessions`)

A project's pane (Mac, iPad) or screen (iPhone), and Chats, list their tasks with the Board's
rows (`TaskListRow`, §4.8), filtered to the project: the archive circle, the three glyphs, an
activity line and the age. The line names the branch, after the machine while the list's
sessions run on more than one machine (`ProjectSessions.spansMachines`), then what the task is
doing; a waiting dot replaces the old Needs you section.

- **Sections** come from the sort menu (`BoardSortMenu(BoardSort.sessions)`, kept in
  `sessionsSort`, apart from the Board's): Recent, the default, a section a day of last
  activity; Created, a section a day of the top session's ULID; Name, one untitled run by title.
- **Archived · N** ends the list, collapsed; it opens to the archived tasks, dimmed, their circle
  filled.
- **Archiving** works as on the Board: the circle, a swipe on iOS or the context menu archives
  the top session and so its tree, with the toast's Undo and ⌘Z.
- **Header:** the project's tile (`ProjectIcon`), name, and its live status rather than its
  machines (`ProjectSessions.status`): "1 waiting for you · 3 working · 156 sessions", after
  the rolled-up state glyph, with the sort menu, settings and New Session. The iPhone's Projects
  list says the same status under each project's name.

### 4.3 Pull Requests

The apps have no Pull Requests section (unlike the TUI's prs view), and nothing counts PRs
across sessions. A session shows its PRs as text: the chip in its header (§5.1) and the badges
on its row.

### 4.4 Machines and vault (`MachinesView`, `VaultView`)

A grid of `MachineCard`s (adaptive, at least 360 pt per column, so one column on iPhone): name,
connection, role, running and total sessions, then each account with its usage windows
(`UsageBar`). The card's gear opens `MachineSettingsSheet`. Machines fills the width on the Mac
(no list beside it). A vault shows its hosts (`VaultSection`) here.

Vault shows each paired vault's `VaultSection`. A vault that connected machines back up to
(`get_vault_link`, asked of the machines this device owns) but this device is not paired with
gets an `UnpairedVaultCard` instead: who backs up to it, and how to pair with it to see its
statistics.

A session a paired vault replicates is listed once (`Lists.shadowed`): from its own host while
that host is paired here and connected, else from the vault, read-only. Only the listed copy is
followed, so pairing a vault does not stream every session twice.

In `MachineSettingsSheet` an owner backs a connected machine up (`BackupField`): it shows
where the machine backs up (`get_vault_link`), backs it up to a connected vault this device owns
(`pair_vault_host` on the vault, then `link_vault` on the machine), or stops (`unlink_vault`,
then `revoke_vault_host` on the vault when it is paired here as its owner).

In `MachineSettingsSheet` an owner adds an account (`AddAccountSheet`) or edits one
(`EditAccountSheet`). **Start Login** and the edit sheet's **Log In Again** (the provider's
login of that account again, in its own config directory, for a login that expired) start the
login in Activity (§4.9) and close the sheet; no sheet shows a terminal.

Providers › **Update All**, a CLI's Install or Update, and **Install & Set Up** queue each job in
Activity with `Fleet.install`. The daemon checks the CLI's version again before it closes an
install's terminal, so the machine's versions are current when the job ends.

### 4.5 Usage (`UsageView`)

Modelled on T3 Code's Usage page. A period picker (24h, 7d, 30d, month to date) and a machine
filter, all machines added up by default (`UsageReport`, from each connected machine's
`get_usage_summary`; a vault is not asked). Then the totals as tiles: API-equivalent cost
("≈" and "partly estimated" when herder priced some of it), tokens, cache savings (the share of
input read from the prompt cache), input, output, cache read and write. Then one card per
account, costliest first: provider, tokens and dollars for the period, and what is left of its
plan's session and weekly windows with their reset (`UsageBar`, the windows Machines shows).
Last, a by-model table of turns, tokens and dollars. It fills the width on the Mac, like
Machines; on iPhone it is the Usage tab. Screenshots are still to come, in `docs/screenshots/p11-4`.

### 4.6 Skills (`SkillsView`)

The skill library (`SkillLibrary`, from each machine's `skills_status`). At the top, the
repository, or "Kept on each of your machines" until one is set, with **Pull** (with a
repository), **New Skill** and **Import**. Then one card per skill: name, description, the
providers it reaches, its menu (Edit, Delete) and a switch per machine (`set_skill_enabled`).
Without a repository, a write goes to every connected machine the user owns, each keeping its
own library, and a card offers to sync through a git URL (`set_skills_repo`, through one
machine the user owns; the client sets it on the rest, and each pushes the skills it has).
With one, a write goes through one machine and the others pull, and one row per machine shows
its commit, when it last pulled, and why a pull failed. **New Skill** and **Edit** write one
`SKILL.md` (`SkillEditorSheet`: name, description, instructions); editing replaces the skill's
folder, since the app cannot read a skill's files. **Import** (`SkillImportSheet`) takes a
folder picked on the device, every file in it but the hidden ones, named after it
(`SkillFolder`), or a git URL and a folder in it. **Account Skills** lists, read-only, the
skills in each account's own folder (`~/.claude/skills`, with those Claude syncs from
claude.ai), per account and machine. Last, **Project Skills**: the skills checked in to the repository
of the session open beside the screen, or of a project picked from a menu, read-only and
labelled with their folder. Members see everything and change nothing: the write controls are
hidden and the switches disabled. It fills the width on the Mac; on iPhone it opens from the
top of Machines. Screenshots are still to come, in `docs/screenshots/p11-9`.

### 4.7 List rules (every session list)

- **Task lists:** the Board (§4.8), a project's list and Chats (§4.2) list top-level task
  trees, one `TaskListRow` each, sectioned a day each by the sort menu; a child has no row of its
  own, its parent's says "N agents" and it opens from the session (§5.6).
- **Archiving hides a session, never its live children:** each tree is built from all of a
  project's sessions before the list splits them. A tree with any session that is not archived
  is listed with the live ones, without its archived sessions, except an archived parent, which
  stays, dimmed, to lead its live children. A parent's child count counts all its children.
- **Archived last and collapsed:** a project's and Chats' archived tasks go in **Archived · N**
  at the end, collapsed; opening it shows them, dimmed, each archived child no live session
  hangs from as a tree of its own. Archived sessions with no project are not listed.
- **One task row:** `TaskListRow` in every task list: archive circle, state glyph, title, a
  context (the Board's project, a project's machine and branch) · activity line, "N agents" and
  the age. A row opens its session into the selection on Mac and iPad, or pushes it on iPhone.
  `SessionRow` remains for the iPhone's Projects search results.
- **Search:** the pane's search field filters by title, branch, worktree, project, machine and
  PR number or title (`SessionSummary.matches`).

### 4.8 Board (`BoardView`, `Lists.board`)

A task list you clear by archiving. On iPhone, a connection line ("1 machine connected",
`ConnectionLine`) heads it; on the Mac and iPad the page header's status says "9 open · 3 working",
with the machines after it while one is not connected. With no machine paired it offers Add
Machine.

- **One list of top-level task trees,** sectioned by the day of their last activity (Today,
  Yesterday, then the date; Earlier for a tree whose events have not loaded), newest first
  (`BoardSection`). The sort menu (`BoardSortMenu`, kept in `boardSort`) offers Recent, the
  default; Created, by the top session's ULID; and Project, a section a project. A child never
  has a row: the row says "N agents" for its herder children and provider agents, and they open
  from the session.
- **Row** (`TaskListRow`, shared with §4.2): the archive circle, the state glyph, the title,
  `project ·` what the task is doing, "N agents", and the age of its last activity.
- **Three states** (`TaskState`), read from the tree's sessions only, never from a PR, CI or a
  review: working ◐ (`Theme.running`) while one runs or waits for capacity; waiting for you, a
  small amber dot, while one waits on an approval or a question or stopped on a failed turn, the
  line then saying what it waits for; idle ○ otherwise. There are no request cards on the Board:
  approvals and questions are answered in the session (§5.3); the sidebar's Board badge counts
  them.
- **Archiving completes a task:** the circle, E on the Mac, a swipe on iOS, or the context menu
  archives the top session, and with it the tree (the daemon archives children first). The row
  goes at once; the toast's Undo stays 5 s, and ⌘Z (the window's undo manager) undoes too.
  "Archived today · N" ends the list, with Show Archived listing the tasks archived today,
  dimmed, their circle filled: it or a swipe brings one back.
- **Mac keyboard:** J and K move the highlighted row, Return opens it beside the list, E
  archives it; a legend under the list says so.
- **Search** keeps a tree when its top or any of its children matches.

### 4.9 Activity (`Activity`, `ActivityChip`, `ActivityPanel`)

One place for everything long-running this device started: CLI installs and updates, account
logins and Log In Again, AWS SSO sign-ins (`AwsSignInSheet` only picks the profile and machines,
then hands each machine's sign-in to Activity). Starting one never opens a sheet. Jobs live
while the app runs (`Fleet.activity`), each in its own terminal on its machine, and run whether
their terminal is on screen or not.

- **Indicator.** On the Mac and iPad, `Pane` puts `ActivityChip` in the `PageHeader`, between
  the status and the actions: "Activity · N" (N jobs not ended) with a ring of the jobs done,
  amber while one needs the user; a ✓ (or a red ! after a failure) once all have ended. Hidden
  with no jobs. On the iPhone, `ActivityToolbarButton` in each tab's toolbar opens the same
  list as a sheet (`AppSheet.activity`).
- **List** (`ActivityList`): grouped as Needs you (a login waiting for its browser login, an AWS
  code to approve), Running, Queued and Done (newest first). Each row: a title that says the
  outcome once ended ("Update Codex on work-mbp", "Updated Claude on work-mbp", "OpenCode on
  trash-can-01 failed"), a detail line (the versions, the step, or why it failed) and its
  action: **Open login**, **Approve**, or **Retry**. **Clear finished** clears Done.
- **Detail** (`ActivityDetail`): the outcome chip, the warnings read out of the vendor's output
  (`VendorWarning`: a second, npm-installed Claude, with the command that removes it to copy),
  Cancel Login or Retry, the terminal and **Copy log** (`TerminalConnection.log`, the output
  without its escape sequences).
- **Queue** (`Activity.startable`): installs on the same machine run one at a time, as two
  package managers writing one prefix trip over each other; different machines run at once.
  Logins and sign-ins start at once: they wait on the user and would hold the queue.

Screenshots: [`screenshots/p14-6/activity.png`](screenshots/p14-6/activity.png)
(`ActivityScreenshotTests`); `mac-activity-queued.png`, `mac-activity-done.png`,
`mac-activity-detail.png`, `mac-activity-login.png`, `ios-activity-login.png` from
ScreenshotTests.

---

## 5. Session (`SessionView`)

### 5.1 Header

From the top: a child's `ChildBanner` ("Agent of ‹parent›", Back), then the header:

- **Left, stacked:** the title (`.title3` bold, two lines at most, then truncated), the status
  line (`StatusGlyph` + state label + `· project · machine`), the branch (monospace, one line,
  middle-truncated), and "Forked from …" when it is a fork.
- **Right, one row:** the PRs chip (when linked), then `HeaderButton`s: Terminal / Chat (owners only, ⌘\`),
  Inspector (ⓘ, ⌥⌘I), list toggle (Mac, ⌘\), and the ⋯ menu (Interrupt, Switch Account or
  Model…, Archive).
- **The PRs chip rolls up the session's tree** (`PRRollup`): its own PRs and every
  descendant's (children, grandchildren), a PR linked to two of them counted once. It is one
  chip, never one per PR: a single PR shows its number ("#181"), more show the count and how
  many are open ("16 PRs · 3 open"), tinted by the most urgent state (open, draft, merged,
  closed). Where the header drops its labels it shortens to open of all ("3/16"). It is plain
  text (`HeaderLabel`): it opens no list.
- **On a phone** the header must fit 375 pt: the title takes the remaining width and wraps to
  two lines; the status line and branch truncate rather than wrap; the buttons drop their labels
  to icon-only before anything truncates (no "Ter…"); a child's left edge carries a 3 pt
  `Theme.child` bar.

### 5.2 Transcript

- The content column is at most 760 pt, centred, with 16 pt margins (`SessionView`'s
  `LazyVStack`), so it reads the same on a 27" display and fits a phone with no horizontal
  scroll. Nothing inside may force a width: code blocks and tables scroll horizontally inside
  their own box (`MarkdownText`, `MarkdownTable`, cells capped at 320 pt).
- User messages are right-aligned bubbles (`Theme.bubble`, 18 pt corners) with 48 pt of space
  on the left. Assistant text is unframed markdown. Tool calls group into one quiet card of
  one-line rows (`ToolGroup`, `ToolRow`), reasoning is one italic line, notices are rules across
  the column (`NoticeLine`), as the TUI renders them.
- The transcript follows new output from the bottom; scrolled up, it stays put and returns to
  where it was left (`TranscriptScroll`, `FollowsGrowth`).

### 5.3 Requests

The oldest approval, else the oldest question, pins above the composer as a `RequestCard`
("+N more" when there are others), with large Deny / Allow or numbered choices (48 pt), as the
TUI pins its request panel. It never replaces the composer: a question can also be answered by
typing.

In the transcript, each question is a `QuestionRecord` card where it was asked: its text,
rendered as Markdown, with "Waiting for you" (accent border) until it is answered. After that,
the card lists the choices with what each means and checks the one picked, or shows the typed
answer. An approval stays a line across the transcript.

### 5.4 Composer and its toolbar (`ComposerBox`)

- A rounded box (22 pt corners, `Theme.surface`): the prompt on top (2–12 lines on iOS;
  `PromptEditor` on the Mac), then inside its bottom edge the model menu (`ModelPicker`), the
  permission menu, dictation, attach (Mac; paste and drop everywhere) and the send button, which
  turns into stop while a turn runs with an empty prompt (`CircleButton`).
- Under it, a footer bar (`FooterMenu`): machine, account, branch. A child session tints the box
  edge with `Theme.child`.
- The column is the transcript's plus 24 pt (784 pt), centred, 12 pt from the window edges.
- **Focus:** on the Mac the prompt is focused when a session opens. On iPhone it is not: the
  session opens on its transcript, and the keyboard appears when the user taps the prompt.
  Dragging the transcript dismisses the keyboard.
- Read-only sessions (moved, or on an offline host) show a lock line in its place.
- **Skill mentions:** `$` starting the prompt's last word opens a picker (`SkillPicker`) at the
  top of the box with the session's skills (`session_skills`; a draft offers the machine's enabled
  library skills and its account's own), names starting with what follows the `$` first, each
  with its description and Library, Project or Account. A bare `$` with no skills to pick says
  so, pointing to Skills. A click or Return picks one; it goes in as `$name `, the text the agent
  gets, which the adapters rewrite per provider. A mention shows as a chip with its description:
  inline on the Mac (`PromptEditor`, as images and pastes), and above the text on iOS
  (`SkillStrip`), whose text view keeps the mention as typed.

### 5.5 Queue (`QueueTray`)

Queued messages stack above the composer in run order, four rows visible (34 pt rows on the
Mac, 44 pt on iOS), each with send now, remove, and drag to reorder; swipe to remove on iOS.

### 5.6 Children

`ChildrenCard` in the transcript lists a parent's children with their state; tapping one opens
it (pushed on iPhone, in place on Mac/iPad). `ChildReportCard` shows a child's report back.

### 5.7 Terminal (`TerminalPane`)

Owners only, as in the TUI. Terminal replaces the transcript and composer inside the session
view, on every size class, with its shell tabs on top; Chat switches back. On iOS a key bar
(esc, ctrl, arrows) sits above the keyboard.

### 5.8 New session (`ProjectPicker`, `DraftSessionView`)

New Session opens the project picker: search, one row per project, then "Other repository…".
Picking one opens the draft, an empty chat with the composer preselected for that project's
machine, model and permission mode; the first message creates the session and the draft becomes
it. The draft shows in the session pane on Mac/iPad and full screen on iPhone.

Screenshots: `mac-session.png`, `mac-session-approval.png`, `mac-session-running.png`,
`mac-session-terminal.png`, `ios-session.png`, `ios-session-keyboard.png`,
`ios-session-approval.png`, `ios-session-running.png`, `ios-session-menu.png`.

---

## 6. Secondary panes, sheets and popovers

| pane | Mac / iPad regular | iPhone / compact | component |
|---|---|---|---|
| Inspector (events, stats) | 300 pt side pane right of the chat, toggled from the header | sheet, medium and large detents | `SessionInspector` |
| Terminal | replaces the transcript | replaces the transcript | `TerminalPane` |
| Model, account, machine | popover (340 pt) | popover (fits 375 pt) | `ModelPicker`, `FooterMenu` |
| Permission mode, ⋯ | menu | menu | SwiftUI `Menu` |
| New Session | sheet | sheet, full width | `ProjectPicker` |
| Machine and project settings, Add Machine, Switch, Fork | sheet, 560 pt (`SheetScaffold`) | system sheet, full width | `SheetScaffold` |
| Draft session | session pane | full screen | `DraftSessionView` |
| Activity (installs, logins, sign-ins) | popover (560 pt) from the header chip | sheet from the toolbar | `ActivityPanel` |
| Toasts (archived, undo) | bottom, centred capsule | same | `ToastView` |

Rules:

- **A side pane never takes width from a phone.** On compact width any pane that is a side pane
  on regular width opens as a sheet instead.
- **Fixed widths only on regular width.** A sheet or popover with a fixed width applies it
  under `#if os(macOS)` or a regular size class only (`SheetScaffold` does this already).
- **Every sheet uses `SheetScaffold`**: title and subtitle, a close button (Esc on the Mac),
  scrolling content, a footer with the primary action on the right.

Screenshots: `mac-session-inspector.png`, `ios-session-inspector.png` (today's side pane on a
phone), `mac-new-session.png`, `ios-new-session.png`, `mac-machine-settings.png`,
`ios-machine-settings.png`.

---

## 7. Touch targets and input

- **iOS and iPadOS:** every tappable control has at least a 44 × 44 pt hit area. Visual size can
  be smaller (a 32 pt `HeaderLabel` with 6 pt of `contentShape` padding around it); hit size
  cannot. Already compliant: `ActionButton` (48), `ChoiceButton` (48), `InputBox` and
  `ChoiceChips` (44), the iOS `QueueTray` rows (44), `ListAndSession`'s Back (44).
- **Mac:** controls are 28–34 pt (`IconButton` 30, `HeaderLabel` 32, `SidebarRow` 34), with
  hover states, tooltips (`.help`) and a keyboard shortcut for every frequent action: ⌘N new
  session, ⌘R reconnect, ⇧⌘\ sidebar, ⌘\ list, ⌘\` terminal, ⌥⌘I inspector, ⌘[ parent, ⌘↩ send,
  ⇧⌘D dictate.
- **Labels never wrap inside a button.** A labelled button that does not fit drops to its icon
  (`ViewThatFits`), keeping the label as its accessibility label.
- **Hover-only actions have a touch equivalent:** the archive button on hover (`SessionLink`) is
  a swipe and a context menu on iOS; the settings gear on hover (`SidebarRow`) also shows on the
  selected row, which is how iPad reaches it.
- **Every icon-only button has an accessibility label** (`IconButton.help`, `HeaderButton`).

---

## 8. Design tokens (`Design/`)

### 8.1 Colour (`Theme`)

| token | dark | use |
|---|---|---|
| `background` | `#0A0A0A` | window, transcript |
| `surface` | `#141414` | sidebar, cards, composer, sheets |
| `raised` | `#1A1A1A` | chips, code blocks, secondary buttons, selection |
| `stroke` | `#252525` | dividers, card borders |
| `text` / `secondary` / `tertiary` | `#F7F7F7` / `#D1D1D1` / `#939393` | three text levels |
| `primaryFill` / `primaryStroke` | `#26262B` / `#3D3D44` | the primary button: dark raised fill, lighter border |
| `bubble` / `onBubble` | `#1A1A1A` / `#F7F7F7` | user messages, header buttons |
| `accent` | `#E69F00` | needs you: approvals, questions (TUI `attention`), and nothing else |
| `running` | `#5382EE` | running (TUI `warning`) |
| `waiting` / `idle` | `#999999` / `#666666` | waiting for capacity / idle |
| `failure` | `#D55E00` | error, closed PR, offline host |
| `success` | `#009E73` | passing CI, open PR, done |
| `merged` | `#CC79A7` | merged PR |
| `child` | `#9D8CF0` | child sessions: banner, header edge, composer edge |

Accent, failure, success and merged are the Okabe–Ito colours, distinguishable with colour
blindness. No view uses a literal colour; new colours are added to `Theme` first.

### 8.2 Type, shape, spacing

- Type is the system font at Dynamic Type text styles (`.body`, `.subheadline`, `.footnote`,
  `.caption`); monospace for code, branches and commands (`Theme.mono`, `Theme.monoSmall`).
  Fixed point sizes only for glyphs and the draft's 30 pt question.
- Corners: `Theme.corner` (10 pt) for cards and fields, 8–9 pt for buttons, 18 pt bubbles,
  22 pt composer, capsules for chips and badges.
- Spacing: 16 pt screen margins on iOS, 20 pt pane margins on the Mac; 22 pt between
  groups; content columns 760 pt (transcript, Board) and 784 pt (composer).

### 8.3 Components

| component | file | rule |
|---|---|---|
| `StatusGlyph` | `Design/StatusGlyph.swift` | the state set: running (pulsing blue dot), needs you (accent `!`), waiting (dashed ring), idle (ring), error (`✕`), archived (box), moved (`→`); same order and meaning as the TUI's §3.2 |
| `PRBadge` | `Design/StatusGlyph.swift` | number coloured by PR state, CI mark, `!` for conflicts or changes requested |
| `UsageBar` | `Design/StatusGlyph.swift` | neutral under 70%, accent from 70%, failure from 90%, as the TUI |
| `SessionRow` | `Design/SessionRow.swift` | the one session row (§4.7) |
| `RequestCard`, `ActionButton` | `Design/RequestCard.swift` | approvals and questions; 48 pt buttons |
| `PageHeader`, `SignalStrip`, `SettingsList`, `SortableTable`, `RelativeTime` | `Design/` | the design system (§8.4) |
| `SheetScaffold`, `Field`, `InputBox`, `ChoiceChips`, `DetailRow` | `Design/Sheet.swift` | every sheet and form |
| `Card`, `Chip`, `SectionHeading` | `Design/Theme.swift` | surfaces and headings |
| `ProjectIcon`, `ProviderLogo` | `Design/` | a project's tile; a provider's mark |
| `Pane`, `PaneButton`, `IconButton`, `Sidebar` | `DesktopShell.swift` | the regular-width frame; `PaneButton(prominent:)` is a page's main action |
| `HeaderButton`, `HeaderLabel` | `SessionView.swift` | the session header's buttons |

### 8.4 Design system (P14.2)

The rules every screen follows from P14 on, and the component that carries each. A screen todo
adopts them; nothing new is drawn by hand where a component exists. HerderKit's
`DesignSystemScreenshotTests` draws them all on one sheet
([`screenshots/p14-2/design-system.png`](screenshots/p14-2/design-system.png)); each has an
Xcode preview in its file.

- **The header says what is true now** (`PageHeader`, which `Pane` draws). A title at the page
  size, a live status sentence built from the page's state ("9 open · 3 working"), then the
  page's actions, the main one `PaneButton(prominent: true)`. No subtitle explains what the
  page is for: that goes in the empty state or an ⓘ (`InfoButton`). The Activity chip (§4.9)
  sits between the status and the actions.
- **Show signal, hide zeros** (`SignalStrip`). A chip per state that is non-zero, coloured by
  its `Urgency`: needs you (amber), failing (red), running (blue), done (green), quiet (grey).
  A chip filters the page to its state; the chosen one clears the filter. With every count
  zero the strip takes no room. Totals go at the bottom or on hover, not in the strip.
- **Three surfaces.** The canvas (`Theme.background`) under every page; a section is a
  `SectionHeading` over rows split by hairlines, with no box; the raised surface
  (`Theme.raised`) for a row's hover and selection and a settings group. A card
  (`Card`, `RequestCard`) only for an object you act on: a request, a machine in its detail page.
- **Settings are rows** (`SettingsList`, `SettingRow`). Grouped under a heading; the label on
  the left with at most one line of hint, the control on the right. Longer help goes behind
  the ⓘ (`SettingRow(help:)`).
- **Collections are tables** (`SortableTable`). Anything over about four items of one kind.
  A click on a column header sorts by it, again reverses it, and the sorted column shows an
  arrow. Each device keeps each table's sort (`table.<id>.sort` in its defaults). Text sorts as
  Finder does. Where order is personal, rows reorder by dragging instead.
- **Relative time** (`RelativeTime`). "3 min. ago", "yesterday", kept current, with the exact
  time on hover on the Mac and on a long press on iOS. No ISO timestamp reaches the UI.
  Monospace (`Theme.mono`) only for versions, branches, ids and code.
- **Type scale** (`Theme.page`, `.title`, `.body`, `.meta`): 20 page · 14 section and row
  title · 13 body · 11.5 meta (times, counts, hints) on the Mac; on iOS the nearest Dynamic
  Type styles (title 2, headline, subheadline, footnote).
- **One primary button style, never white** (`View.primaryFill(in:on:)`). The dark raised fill
  (`Theme.primaryFill`) with a lighter border (`Theme.primaryStroke`) and semibold
  `Theme.text`. `ActionButton(.primary)`, `PillButton(prominent:)`, `PaneButton(prominent:)`, the
  send button, selected choice chips and selected header buttons all draw it. A page has one
  primary action; the rest are `Theme.raised` secondary buttons.
- **Amber means needs you** (`Theme.accent`), and nothing else uses it.
- **Every problem carries its fix.** Anything warning- or error-coloured has its action
  button beside it.

---

## 9. Screenshots

[`docs/screenshots/p7-20`](screenshots/p7-20) has every main screen as `main` draws it today,
against a fake daemon with sessions in every state: the Mac at 1432 × 859 pt (`mac-*.png`) and
an iPhone 18 Pro simulator (`ios-*.png`). They show today's state, including the gaps below; the
iPhone layout fixes land in P7.21.

| screen | Mac | iPhone |
|---|---|---|
| Board | `mac-home.png`, `mac-sidebar-expanded.png` | `ios-home.png` |
| Project / Projects | `mac-project.png` | `ios-projects.png` |
| Machines, machine settings | `mac-machines.png`, `mac-machine-settings.png` | `ios-machines.png`, `ios-machine-settings.png` |
| Session (idle, approval, running) | `mac-session.png`, `mac-session-approval.png`, `mac-session-running.png` | `ios-session.png`, `ios-session-keyboard.png`, `ios-session-approval.png`, `ios-session-running.png` |
| Session menu | — | `ios-session-menu.png` |
| Inspector | `mac-session-inspector.png` | `ios-session-inspector.png` |
| Terminal | `mac-session-terminal.png` | — |
| New session | `mac-new-session.png` | `ios-new-session.png`, `ios-new-session-path.png` |
| Narrow window | `mac-narrow.png` | — |

---

## 10. Where the apps differ today


None known: every gap found when this spec was written (P7.21–P7.27) is closed.
