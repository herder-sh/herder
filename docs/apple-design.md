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

1. **One information architecture everywhere.** Home, Projects, Pull Requests, Usage, Skills,
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
| **Home** | needs-you cards, then Active and Recent sessions across all machines | inbox + attention list | `HomeView` |
| **Projects** | every project with its sessions, children under parents | sidebar *projects* tree | `ProjectsView` (iPhone), `ProjectSessions` (Mac/iPad) |
| **Pull Requests** | PRs linked to sessions, by project | prs view | `PullRequestsView` |
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

- Sections are written in title case in the chrome ("Pull Requests", "New Session") and as
  `SectionHeading` capitals inside panes ("NEEDS YOU 2", "ACTIVE", "RECENT", "ARCHIVED").
- The product is always lowercase `herder`, including the iPhone Home title.
- A session is named by its title; its branch shows in monospace (`Theme.monoSmall`) as a
  secondary line, never instead of the title.
- A child session is "Agent of ‹parent›" (`ChildBanner`), as the TUI's task child.

---

## 3. Size classes and navigation

| | Mac window / iPad regular | narrow regular (< 820 pt content) | iPhone / compact |
|---|---|---|---|
| frame | `DesktopShell` | `DesktopShell` | `FleetView.tabs` |
| navigation | sidebar: rail (76 pt) or full (228 pt) | same | tab bar: Home · Projects · PRs · Usage · Machines |
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
- **The sidebar** (`Sidebar`, `SidebarRail`) lists Home, Pull Requests, Usage, Skills, Machines, Vault (when
  a vault is paired), then the projects, then each machine's connection at the bottom. It starts
  collapsed to the rail (`sidebarCollapsed`), which leaves the width to the session; ⇧⌘\
  toggles it. The window's traffic lights sit in its top bar (`Sidebar.topBar`, 52 pt on the
  Mac).
- **The list pane** is 380 pt (`ListAndSession`). ⌘\ hides it to give the session the whole
  width (`listHidden`); with no session open it always shows.
- **The pane header** (`Pane`) is the title (a menu of sections and projects, `Switcher`), a
  one-line subtitle, the pane's actions on the right (`PaneButton`, `IconButton`), then a
  search field where the pane lists sessions.
- **The tab bar** on compact width has the same sections as the sidebar: Home (badged with the
  needs-you count), Projects, PRs, Usage, Machines. Skills and a vault show inside Machines. Each tab is its own
  `NavigationStack` (`homePath`, `projectsPath`, `prsPath`), so switching tabs keeps each tab's place.
- **Back always returns to where the user came from**: the stack's back button on iPhone,
  `ListAndSession`'s Back on narrow regular, `ChildBanner`'s Back (⌘[) from a child to its
  parent.

Screenshots: `mac-home.png` (rail), `mac-sidebar-expanded.png`, `mac-narrow.png`,
`ios-home.png`.

---

## 4. Lists and sections

### 4.1 Home (`HomeView`)

1. **Needs you**: one `RequestCard` per pending approval or question, newest first, answerable
   in place. The card's session line opens that session.
2. **Active**: running, waiting and needing-you sessions without a card, as task trees in
   creation order so the list holds still while they work (`Lists.home`).
3. **Recent**: idle and failed sessions, newest activity first, at most 20.

Home never lists archived sessions. On iPhone, a connection line ("1 machine connected",
`ConnectionLine`) heads the list; on the Mac and iPad that line is the pane subtitle.

### 4.2 Projects (`ProjectsView`, `ProjectSessions`)

One group per project, sorted by name, "No project yet" last. The group header is the
project's tile (`ProjectIcon`), name, machines, and its New Session and settings buttons. Inside:
the live sessions as task trees, then **Archived** (§4.7).

### 4.3 Pull Requests (`PullRequestsView`)

Open (default) or All, grouped by project, each session with its PRs (`PRRow`); tapping the
session opens it. On iPhone it is the PRs tab.

### 4.4 Machines and vault (`MachinesView`, `VaultView`)

A grid of `MachineCard`s (adaptive, at least 360 pt per column, so one column on iPhone): name,
connection, role, running and total sessions, then each account with its usage windows
(`UsageBar`). The card's gear opens `MachineSettingsSheet`. Machines fills the width on the Mac
(no list beside it). A vault shows its hosts (`VaultSection`) here.

In `MachineSettingsSheet` an owner adds an account (`AddAccountSheet`) or edits one
(`EditAccountSheet`). The edit sheet's **Log In Again** runs the provider's login of that
account again, in its own config directory, for a login that expired; both show the login
terminal in the sheet (`TerminalSurface`), and the login keeps running if the sheet closes.

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

The skill library (`SkillLibrary`, from each machine's `skills_status`). First run, when no
machine has a library, is one card asking for its git URL (`set_skills_repo`, through one
machine the user owns; the client sets it on the rest). Then the repository, with **Pull**,
**New Skill** and **Import**; one row per machine with its commit, when it last pulled, and why a
pull failed; and one card per skill: name, description, the providers it reaches, its menu
(Edit, Delete) and a switch per machine (`set_skill_enabled`). **New Skill** and **Edit** write
one `SKILL.md` (`SkillEditorSheet`: name, description, instructions); editing replaces the
skill's folder, since the app cannot read a skill's files. **Import** takes a git URL and a
folder (`SkillImportSheet`). Last, **Project Skills**: the skills checked in to the repository
of the session open beside the screen, or of a project picked from a menu, read-only and
labelled with their folder. Members see everything and change nothing: the write controls are
hidden and the switches disabled. It fills the width on the Mac; on iPhone it opens from the
top of Machines. Screenshots are still to come, in `docs/screenshots/p11-9`.

### 4.7 List rules (every session list)

- **Order:** a session that needs you, then running and waiting, then idle and failed; newest
  activity first within each, except Active, which holds creation order (above).
- **Children under parents:** a child is indented under its parent with a tree line
  (`SessionRow.depth`, `TreeLine`, 14 pt per level); a parent row shows its child count and how
  many need you. Idle children stay under their parent and never crowd Home.
- **Archived last and collapsed:** archived sessions go in an **Archived** group at the end of
  their project, collapsed, showing a count; opening it shows them, dimmed. Children stay under
  their parent within each group. Archived sessions with no project are not listed.
- **One row component:** `SessionRow` everywhere: glyph, title (one line), age, activity (one
  line, accent when it needs you), project · branch, PR badges (two at most, then `+N`),
  machine. Rows are wrapped by `SessionLink` (Mac/iPad: selection, hover archive button,
  context menu) or a `NavigationLink` with a trailing swipe to archive (iPhone).
- **Search:** the pane's search field filters by title, branch, worktree, project, machine and
  PR number or title (`SessionSummary.matches`).

---

## 5. Session (`SessionView`)

### 5.1 Header

From the top: a child's `ChildBanner` ("Agent of ‹parent›", Back), then the header:

- **Left, stacked:** the title (`.title3` bold, two lines at most, then truncated), the status
  line (`StatusGlyph` + state label + `· project · machine`), the branch (monospace, one line,
  middle-truncated), and "Forked from …" when it is a fork.
- **Right, one row of `HeaderButton`s:** PRs (when linked), Terminal / Chat (owners only, ⌘\`),
  Inspector (ⓘ, ⌥⌘I), list toggle (Mac, ⌘\), and the ⋯ menu (Interrupt, Switch Account or
  Model…, Link Pull Request…, Archive).
- **The PRs button rolls up the session's tree** (`PRRollup`, `PRStrip`): its own PRs and every
  descendant's (children, grandchildren), a PR linked to two of them counted once. It is one
  chip, never one per PR: a single PR shows its number ("#181"), more show the count and how
  many are open ("16 PRs · 3 open"), tinted by the most urgent state (open, draft, merged,
  closed). Where the header drops its labels it shortens to open of all ("3/16"). Its list must hold a hundred PRs: an Open / All filter (Open while any is open),
  grouped by session (this one first, then each descendant by title, nested), each group open,
  draft, merged, closed, with merged and closed folded behind "n merged"; a search (number,
  title, branch) once there are more than 10; one-line rows (`PRLine`: number, title truncated,
  state, CI) in a capped, lazily built scroll. A descendant's group title opens that session.
  The Pull Requests section (§4.3) stays per session.
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
  library skills), names starting with what follows the `$` first, each with its description and
  Library or Project. A click or Return picks one; it goes in as `$name `, the text the agent
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
| Session PRs (with descendants', §5.1) | popover under the header button, 520 pt, scroll capped at 460 pt | sheet | `PRStrip` |
| Model, account, machine | popover (340 pt) | popover (fits 375 pt) | `ModelPicker`, `FooterMenu` |
| Permission mode, ⋯ | menu | menu | SwiftUI `Menu` |
| New Session | sheet | sheet, full width | `ProjectPicker` |
| Machine and project settings, Add Machine, Switch, Fork | sheet, 560 pt (`SheetScaffold`) | system sheet, full width | `SheetScaffold` |
| Draft session | session pane | full screen | `DraftSessionView` |
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
| `primary` / `onPrimary` | `#F7F7F7` / `#141414` | the primary button |
| `bubble` / `onBubble` | `#1A1A1A` / `#F7F7F7` | user messages, header buttons |
| `accent` | `#E69F00` | needs you: approvals, questions (TUI `attention`) |
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
- Spacing: 16 pt screen margins on iOS, 20 pt pane margins on the Mac; 22 pt between Home
  groups; content columns 760 pt (transcript, Home) and 784 pt (composer).

### 8.3 Components

| component | file | rule |
|---|---|---|
| `StatusGlyph` | `Design/StatusGlyph.swift` | the state set: running (pulsing blue dot), needs you (accent `!`), waiting (dashed ring), idle (ring), error (`✕`), archived (box), moved (`→`); same order and meaning as the TUI's §3.2 |
| `PRBadge` | `Design/StatusGlyph.swift` | number coloured by PR state, CI mark, `!` for conflicts or changes requested |
| `UsageBar` | `Design/StatusGlyph.swift` | neutral under 70%, accent from 70%, failure from 90%, as the TUI |
| `SessionRow` | `Design/SessionRow.swift` | the one session row (§4.7) |
| `RequestCard`, `ActionButton` | `Design/RequestCard.swift` | approvals and questions; 48 pt buttons |
| `SheetScaffold`, `Field`, `InputBox`, `ChoiceChips`, `DetailRow` | `Design/Sheet.swift` | every sheet and form |
| `Card`, `Chip`, `SectionHeading` | `Design/Theme.swift` | surfaces and headings |
| `ProjectIcon`, `ProviderLogo` | `Design/` | a project's tile; a provider's mark |
| `Pane`, `PaneButton`, `IconButton`, `Sidebar` | `DesktopShell.swift` | the regular-width frame |
| `HeaderButton`, `HeaderLabel` | `SessionView.swift` | the session header's buttons |
| `PRLine` | `PullRequests.swift` | one PR on one line in the session's PR list (§5.1) |

---

## 9. Screenshots

[`docs/screenshots/p7-20`](screenshots/p7-20) has every main screen as `main` draws it today,
against a fake daemon with sessions in every state: the Mac at 1432 × 859 pt (`mac-*.png`) and
an iPhone 18 Pro simulator (`ios-*.png`). They show today's state, including the gaps below; the
iPhone layout fixes land in P7.21.

| screen | Mac | iPhone |
|---|---|---|
| Home | `mac-home.png`, `mac-sidebar-expanded.png` | `ios-home.png` |
| Project / Projects | `mac-project.png` | `ios-projects.png` |
| Pull Requests | `mac-pull-requests.png` | [`p7-22/ios-pull-requests.png`](screenshots/p7-22/ios-pull-requests.png), [`p7-22/ios-pull-requests-session.png`](screenshots/p7-22/ios-pull-requests-session.png) |
| Machines, machine settings | `mac-machines.png`, `mac-machine-settings.png` | `ios-machines.png`, `ios-machine-settings.png` |
| Session (idle, approval, running) | `mac-session.png`, `mac-session-approval.png`, `mac-session-running.png` | `ios-session.png`, `ios-session-keyboard.png`, `ios-session-approval.png`, `ios-session-running.png` |
| Session menu | — | `ios-session-menu.png` |
| Session PRs with descendants' (1, 5, 60) | [`p7-28/mac-1.png`](screenshots/p7-28/mac-1.png), [`p7-28/mac-5.png`](screenshots/p7-28/mac-5.png), [`p7-28/mac-60.png`](screenshots/p7-28/mac-60.png), [`p7-28/mac-60-all.png`](screenshots/p7-28/mac-60-all.png), [`p7-28/mac-60-search.png`](screenshots/p7-28/mac-60-search.png) | [`p7-28/ios-1.png`](screenshots/p7-28/ios-1.png), [`p7-28/ios-5.png`](screenshots/p7-28/ios-5.png), [`p7-28/ios-60.png`](screenshots/p7-28/ios-60.png), [`p7-28/ios-60-all.png`](screenshots/p7-28/ios-60-all.png) |
| Inspector | `mac-session-inspector.png` | `ios-session-inspector.png` |
| Terminal | `mac-session-terminal.png` | — |
| New session | `mac-new-session.png` | `ios-new-session.png`, `ios-new-session-path.png` |
| Narrow window | `mac-narrow.png` | — |

---

## 10. Where the apps differ today


None known: every gap found when this spec was written (P7.21–P7.27) is closed.
