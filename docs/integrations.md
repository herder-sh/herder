# Integrations

Status: a proposal and a mock (P30.1). Nothing here is built yet. The mock screens are in the
Apple app, but only inside the demo (`Fleet.isDemo`, `IntegrationsMock.swift`), and they use
canned data. No protocol type, store table or client-core call exists for any of this. The
user reviews this doc and the screens first. The contract todos come after.

## 1. The problem

herder already talks to outside services in four unrelated ways, each with its own screen
and its own record:

| today | what it is | where it runs | its secrets | its screen |
|---|---|---|---|---|
| Slack and Telegram **bots** (`Bot`, `BotKind`) | a chat bridge: people talk to sessions from a chat | one machine (Socket Mode, long polling) | machine secrets (`SecretRef`) | Bots › Bot Settings, `BotPage` |
| **MCP servers** (`McpServerSettings`) | tools that sessions' CLIs get | with each session, on its machine | hub secrets via `herder secret run` | a machine's page, Providers |
| **Skills** | instructions in the library, synced from git | every machine's library | none | Skills |
| **Provider accounts** | the CLIs' logins and endpoints | each machine, per config dir | the vendor CLI's own; herder never reads them | Providers |

Nothing ties these together. You can't browse what's available, install something in one
step, see whether it's healthy, or remove it cleanly. And the things people ask for next
(GitHub, Linear, Jira, Sentry, Discord) are a mix of all of these: a bot, plus tools, plus
events that should start work.

## 2. What an integration is

An **integration** connects herder to one outside service. It's one entry in a catalog, and
an owner installs it once. Under the hood it's made of things herder already knows how to
run, plus one new thing (triggers).

### 2.1 Identity

| field | example | notes |
|---|---|---|
| `id` | `sh.herder.slack`, `io.sentry.mcp` | reverse-DNS, unique in the catalog; the publisher must control the domain or GitHub org (as the MCP registry checks namespaces) |
| `name` | Slack | as the catalog shows it |
| `publisher` | herder · Sentry · `pg-tools` | who stands behind it |
| `trust` | **Built in** · **Verified** · **Community** | built in: ships inside herder, reviewed with it. Verified: herder checked the publisher and pins the package by hash. Community: listed as is, with a warning before install |
| `version` | `1.4.0` | semver; an install pins one |
| `category` | Chat · Code · Issues · Monitoring · Tools | for the catalog's filter |

### 2.2 What it provides (capabilities)

One integration may provide several of these:

- **Chat bridge**: people talk to sessions from the service. These are today's bots,
  unchanged: allowed people, approvals, a session per thread. The conversations stay under
  Bots, next to Chats (apple-design §4.2.1); the integration's page is where you configure it.
- **Tools**: an MCP server that sessions get, in the projects you pick. This is today's
  `McpServerSettings` (stdio or HTTP), with its command or URL taken from the manifest instead
  of typed in.
- **Triggers** (new): the service's events reach herder and start or wake a session. Examples:
  an issue labelled `herder`, a failed check on a herder branch, an assigned Linear issue, a
  PagerDuty incident. A trigger never runs anything itself. It starts a session in a named
  project, with a prompt template, on that project's default machine, and the usual approvals
  apply.
- **Skills**: skill folders the integration adds to the library, such as Figma's
  "build from a frame". They show in Skills like any other skill.

Not in v1: **UI contributions** (custom views, panels, commands in the app). Every
integration's page uses the same anatomy (§5), so a third party never ships UI. This is the
biggest thing we deliberately don't copy from VS Code and Raycast (§7).

### 2.3 Where it runs

| place | for | why |
|---|---|---|
| **One machine** | chat bridges | Socket Mode and long polling connect outward, so no address is needed. This is where bots run today |
| **The hub** | triggers | webhooks need an always-on address, and the hub is the one machine that's meant to be up. It turns an event into a `create_session` on the right machine |
| **With each session** | tools | an MCP server runs where the session's CLI runs, as it does today |

An integration that provides several capabilities runs each part in its own place. For
example, GitHub's triggers run on the hub and its tools run with sessions.

### 2.4 What it needs

- **Secrets** are kept on the **hub**, as named hub secrets (today's `SecretRef`). The app sends
  a value once, when it's typed in the install sheet, and can never read it back. Tools get
  secrets through `herder secret run`. Chat bridges and triggers get them inside the daemon. A
  value never goes into a config file, onto a command line, in front of an agent, or to a
  client.
- An integration **never asks for, stores or relays a provider login**, for Claude, Gemini or
  anyone else. Provider accounts aren't integrations (§4).
- **Scopes** are what the integration may do in the service ("Issues: read and write") and in
  herder. The install sheet shows them, and they're shown again on its page. A new version that
  asks for more scopes waits for the owner to approve it.
- **Reach**: the projects it may start sessions in or give tools to (none means chats only),
  and the machines it runs on. Nothing reaches a project the owner didn't pick.
- **No terminals, ever.** No capability opens a shell. A tool call is a tool call, under the
  session's permission mode and approvals.

### 2.5 Who can do what

- Only **owners** install, configure, update and remove integrations, on the machines they own
  (the hub, for hub-side parts).
- **Members** see what's installed and its health, and use what it provides: they can talk to
  a bot that allows them, and their sessions get the tools. They change nothing, and they never
  see secrets or logs that carry other people's messages. (Is that right? See open question 5.)

### 2.6 Packaging and updates

For v1, herder runs no third-party code inside the daemon:

- **Built-in** integrations (Slack, Telegram, GitHub, Discord) are code in `herder-daemon`, as the
  bots are today. They update when herder updates.
- **Tool** integrations are a manifest that resolves to an MCP server: a pinned package (npm,
  PyPI or OCI image, with a hash) or a remote URL, plus the secrets and scopes it declares. This
  is the shape of an MCP-registry `server.json` entry, so herder can list registry servers
  without inventing a format.
- **The catalog** is a signed index served from herder.sh. It lists the built-ins, the verified
  entries, and a filtered slice of the MCP registry as Community. The app reads the catalog
  through a daemon, not from herder.sh directly, so a fleet without internet still shows what's
  installed.
- **Updates** pin a version per install. The catalog shows "Update available". An owner applies
  it, and a version that asks for new scopes asks first. Nothing updates on its own (open
  question 7).

A WASM plugin runtime (Zed-style) would allow third-party chat bridges and triggers. It's out
of scope until someone needs one that a manifest can't express.

## 3. The catalog

- **One Integrations section** under Workspace on the Mac (sidebar, rail, the title's
  switcher), and a link at the foot of Hub on iPhone, next to Skills and Providers.
- **Installed** first, a row each: the tile, the name, what it provides, where it runs, what it
  did last, and a health chip (Healthy · Needs setup · Failing · Off). A problem also shows up
  in Hub's Needs you, with its fix.
- **Available** next, as cards: the tile, the name, the trust line ("✓ Verified · Sentry"), one
  line of what it does, its capability chips, and **Install**.
- **Search** across name, publisher and summary, plus **category chips** (All · Chat · Code ·
  Issues · Monitoring · Tools).
- **Your own MCP server** is always the last card. It's today's Add MCP Server form, for
  anything the catalog lacks.

## 4. How today's features map onto integrations

The recommendation:

| today | becomes | why |
|---|---|---|
| Slack, Telegram bots | **the chat-bridge part of the Slack and Telegram integrations**. The `Bot` record stays as their config underneath | the same thing, now with install, health and remove. Conversations stay under Bots. Bot Settings becomes the list of installed chat integrations, and each bot's page becomes its integration page |
| MCP servers | **tool integrations**. One from the catalog fills in the command, URL and secrets. A custom one is "Your own MCP server" | the same record (`McpServerSettings`), with a catalog entry on top |
| Skills | **stay separate**. An integration may *add* skills to the library | the library is its own thing (git sync, per-machine switches), and most skills come from no service |
| Provider accounts | **stay separate**, never an integration | they're the CLIs' own logins. Product rule: herder never handles provider tokens, and putting them in a catalog next to third-party code would invite exactly that |

## 5. Flows

- **Install** (`InstallIntegrationSheet`, a sheet): 1. what it will be able to do (the scopes);
  2. where it runs (pick a machine for a chat bridge; the hub or each session otherwise, shown,
  not asked); 3. secrets (stored on the hub); 4. the projects it may touch. Then **Install**.
  It shows in Activity until it's healthy, and its page opens.
- **Configure**: the same sheet with Save, opened from the page's **Configure**. Switches on the
  page save as you go.
- **Health**: each install reports its state, what it last did, and a short log. Needs setup and
  failing each carry their fix ("Add Person", "Replace Token"), on the page and in Hub.
- **Update**: "Update available" on the page and the catalog card. New scopes ask first.
- **Remove**: from the page's Danger zone. It stops the integration everywhere and removes its
  config. Its secrets stay on the hub until deleted, with a link to them.

The integration page (`IntegrationPage`, a `DetailPage`): the header (the tile, the name, the
health chip, "Verified · Sentry · v2.1.0 · With each session", Configure or Install), Needs
attention, What it provides, What it can access (scopes, secrets, projects), Settings, Machines
(or Runs on), Activity, About (id, version, publisher), Danger zone.

## 6. The mock

Inside the demo only. Open the app, choose **Explore a demo**, then go to **Integrations**
under Workspace (Mac) or **Hub › Integrations** (iPhone). The data is in `MockIntegrations`:
Slack (healthy), Telegram (needs setup), GitHub (healthy, on the hub) and Sentry (failing:
token expired) are installed; Linear, Jira, Discord, Playwright, Figma, Postgres, PagerDuty and
Your own MCP server are available. Every action shows "This is a mock: nothing changed".

Screenshots: `integrations-mac.png`, `integrations-mac-narrow.png`,
`integrations-page-{github,sentry,linear}-mac.png`, `integrations-install-mac.png`
(`IntegrationsScreenshotTests`, in the apple workflow's `herderkit-screenshots` artifact), and
`ios-integrations.png`, `ios-integrations-available.png`, `ios-integrations-page.png`,
`ios-integrations-install.png` (`IntegrationsDemoUITests`, in `ios-screenshots-2`).

## 7. How others do it

| | unit | runs | trust | what herder should take | and leave |
|---|---|---|---|---|---|
| **Raycast extensions** | a package with commands, preferences (secrets in the Keychain) | in Raycast's Node process, unsandboxed | every extension is reviewed as a PR into one monorepo | one store with search and categories; declared preferences, where a password type is never shown again | arbitrary UI from extensions; review-everything doesn't scale to the MCP registry |
| **Zed extensions** | `extension.toml` with languages, themes, slash commands, **context servers (MCP)** | WASM sandbox | registry repo; capabilities declared | MCP servers as one kind of contribution; a declarative manifest; pinned versions | a WASM runtime, until there's a need (§2.6) |
| **VS Code** | contribution points (commands, views, settings…) | Node extension host, full access | verified publishers by domain; workspace trust | the verified-publisher badge; asking before trusting | unbounded contribution points and full-access code: herder runs agents with shells, so an extension with full access is too much power |
| **MCP registry** | `server.json`: npm, PyPI, OCI or a remote URL, plus env and secrets | wherever the client runs it | namespace verified by DNS or GitHub | its format for tool integrations, and its namespace check for `id` | listing everything as is; herder labels unreviewed entries Community and warns |

Also worth keeping from Slack's app manifests: scopes listed before install, and new scopes
asked for again on update.

## 8. Open questions for the user

1. **Is "integration" the right name?** We could say "plugin" instead, but plugin suggests
   running third-party code, which v1 doesn't do.
2. **Do triggers belong in v1?** They're the one new capability: webhooks on the hub need the
   hub reachable from the internet (P28 remote access). Without them, GitHub, Linear, Jira and
   PagerDuty are tools only.
3. **OAuth or pasted tokens?** Pasting a token works today and needs nothing new. OAuth
   ("Connect GitHub") needs a callback the hub or herder.sh serves, plus a client id per
   service. If we use OAuth, who hosts it?
4. **Does the catalog come from herder.sh,** fetched through a daemon and signed? Or is it built
   into each release, so it only changes when herder changes?
5. **What can members see?** Installed integrations and their health, surely. But logs can
   carry other people's messages: owners only?
6. **Should Bots stay a section under Chats once Slack and Telegram are integrations?** The
   proposal says yes: conversations live with chats, configuration lives in Integrations, and
   Bot Settings goes away in favour of the integration pages.
7. **Updates for community and verified integrations:** always manual, or automatic for patch
   versions that ask for no new scopes?
8. **Several installs of one integration,** such as two Slack workspaces: one integration with
   several "connections", or one install each?
9. **Should the community slice of the MCP registry be listed at all in v1,** or only built-in
   and verified entries?
10. **Is per-project reach enough,** or do tool integrations need per-session switches too (a
   session that should never get Sentry)?
