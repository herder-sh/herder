# One clear flow for setup, devices and passkeys

A clickable prototype of how a person gets started with herder and how they add machines,
devices, passkeys and teammates. It covers the iPhone (402 × 874) and the Mac (1280 × 800).
This is design only: no app code changes here.

- **Prototype:** open [`index.html`](index.html) in any browser. There is no build step. Use
  the flow chips, ‹ › or ← →, and tap the highlighted buttons inside a screen. The URL hash
  holds the state (`#f=2&s=5&d=both`), so you can link to a single screen.
- **Screenshots:** [`screenshots/`](screenshots) has the key steps for each device, named
  `<flow>.<step>-iphone.png` and `<flow>.<step>-mac.png`. To render them again:
  `npm i playwright && CHROMIUM=/usr/bin/chromium node shots.cjs screenshots [1.5 2.3 …]`.

## What went wrong, in one line each

| today | proposed |
|---|---|
| You claim each machine on its own with `herder claim`, and nothing says what comes next | One guided path: name → machine → approve, with a progress bar and one primary button per screen |
| A fresh phone creates a new passkey and a new person (“Owner”, “Mobile User”) | Welcome asks “new or already use herder?” first. The name is asked once, and every approve screen says **which passkey** it uses and **where it is stored** |
| iOS shows a passkey picker full of stale entries | The `herder://fleet` link carries the passkey’s credential id, so iOS offers only that passkey |
| Join rows show raw addresses and say “Can’t reach it” or “doesn’t know this passkey” | Rows show machine names. Reachability is checked before Face ID. Each failure gives its real cause and a fix: Turn on Tailscale, Use your 1Password passkey, Ask Tomas to add you |
| Fixes need the People matrix, duplicate people and orange errors | Each person gets one role line (“Owner · all machines”). Per-machine roles sit behind “Different per machine”. Duplicates get a one-tap Merge |
| Fleet is full of jargon (“caught up”, fingerprints, “claim”) | Plain status (“Connected”, “Up to date”). Addresses, codes and fingerprints go in secondary text |

## The flows

1. **First launch.** The first screen has two choices. **I’m new** walks through three steps.
   First, your name, used for the person and for the passkey’s label. Second, install on a
   machine: one command, with Kubernetes and “already runs herder” one tap away. On a Mac you
   can pick “This Mac”. Third, approve: scan or paste the code, see the machine’s name and the
   matching code, and approve with **Create Passkey and Approve**. That takes one Face ID. The
   last screen has one primary next step, Start a Session. **I already use herder** goes
   straight to flow 2. An invite link skips the choice altogether (5.4).
2. **Add a device, in one scan.** The giving device shows a large code and lists the machine
   names it shares, plus a line saying the code grants nothing by itself. The receiving device
   shows “Sign in to your 5 machines”, the passkey card (“Tomas · Apple Passwords”) and a
   reachability check for each machine. Then it signs in with **one** Face ID. Each failure
   gives its cause and fix inline. Whatever is left becomes a Needs-you item. The giving
   device shows the same progress live.
3. **Add a machine.** This is Setup’s primary action. One sheet runs Install → Approve → Done.
   It has three tabs: Linux or Mac, Kubernetes, and Has herder (`herder claim`). The Mac
   watches the clipboard for the link and the iPhone scans the code. The Done screen lists
   your other devices. Devices that are signed in as you get a one-tap “New machine: sign in”
   item. Backup to your vault is on by default.
4. **You and your passkeys.** One page has your passkeys (where each is stored, when it was
   added, how many machines know it, and which one this device uses), your devices (each on
   “N of 5 machines”) and Recovery. Add a passkey adds it to every machine with one approval.
   Removing a passkey says what still works afterwards, and the last passkey cannot be
   removed. Recovery is written down in the app, with a “You’re covered” line.
5. **People and access.** The page shows one line per person, with a role menu. Invite sets a
   role (Member by default) and a scope (All my machines, including ones added later), then
   gives a code and a share link. A person’s sheet offers three choices: Owner on all, Member
   on all, or Different per machine. The sheet also shows their devices and Remove. If a
   teammate reaches a machine their invite did not cover, they tap **Ask Tomas to add you**,
   and the request appears in Tomas’s Needs you with an Add button.
6. **Needs you.** This is the top of Setup. It covers machines, devices and people, and every
   item carries the button that fixes it. Amber means needs you. Suggestions such as an update
   or a backup passkey are blue. When everything is fine, the section disappears and the
   status line reads “Everything is set up”.

## What changes in the app

### Screens to add
- **Welcome** (`Launch`/`EmptyFleet` grow into it): the new-or-existing choice and the
  three-step new path with a progress bar. It replaces today’s empty fleet.
- **Setup** section (sidebar Workspace and the 4th iPhone tab). In order: Needs you, You,
  Machines (with the vault as a row), People. Header actions are **Add a device** and **Add a
  machine**.
- **You** page: passkeys, devices, recovery.
- **Passkey** sheet, **Add a passkey** sheet, **Recovery** page.
- **Merge people** sheet.
- **Join from invite** first-launch screen.

### Screens to merge
- **Fleet + People → Setup.** The machine table stays, without jargon. People becomes a
  section with one row per person.
- **People › You + Passkeys → You.**
- **Add to Fleet** (device / machine / vault) **+ Claim your machines + the claim banner**
  become **Add a machine** (with the Has herder tab) and **Add a device**. A vault is added
  from Add a machine; it is a machine with a role.
- **AddDeviceSheet + JoinFleetSheet** stay as two halves of one flow and share the
  per-machine status rows.

### Screens to remove
- The **“Who can do what” matrix** on People. Per-machine roles move into the person’s sheet,
  under Different per machine.
- **“Add a device…” at the foot of Fleet** on the Mac. It is now a header action.
- The **People** sidebar row and the **Fleet** tab name. Both are now Setup.

### Copy
- Never say “claim” in the UI, except as the CLI command `herder claim`.
- Name machines by their name. Addresses and fingerprints go in secondary monospace text.
- Use failure copy that says what happened: “Knows you by another passkey: Tomas in
  1Password”, “Can’t reach it: Tailscale is off on this iPhone”, “Not in your invite”.

### Visual fixes (apply to `docs/apple-design.md` §8)
- **One forward button per screen**: a filled `Theme.action` capsule on iOS and a filled blue
  button on the Mac. Today, §8.4 calls for a dark `primaryFill` primary while sheets use
  `Theme.action`. The proposal: blue for the single forward step in a flow or sheet, and dark
  `primaryFill` for page-header actions only.
- **Sheets**: title and close at the top, scrolling content, and a footer with the hint on
  the left and the primary action on the right. A destructive action sits in the hint’s place
  (Mac) or as a full-width tinted button at the end (iOS). No numbered steps inside sheets
  once a step is done: it collapses to a ✓ line.
- **iOS 26**: glass toolbar buttons, a floating glass tab bar, 24 pt inset groups, 52 pt
  capsule buttons pinned to the bottom (`dock`), and a progress bar on guided flows.
- **Status glyphs** in join and add lists: ✓ green, `!` amber (needs you, with a fix),
  spinner blue, and a dashed ring for waiting. The same set appears on both devices.
- **Empty states** say the next step. An empty Needs you takes no room.

## Needs a contract change (not in this PR)

These parts of the design need protocol or daemon work, each in its own `[CONTRACT]` todo:

1. **Fleet link carries the person and credential id** (`herder://fleet`), so the receiver
   sets `allowCredentials` and shows “Tomas · Apple Passwords” before Face ID. #591 already
   names the machines.
2. **One assertion for many machines**: the client signs a challenge derived from every
   machine’s challenge, and each machine checks its own part. Until then, Face ID asks once
   per machine, and the UI still shows a single button.
3. **“Knows you by another passkey”**: a sign-in refusal tells the client whether the person
   is known by a different credential, the passkey is unknown, or user verification is
   missing (#592).
4. **Access requests** (“Ask Tomas to add you”) delivered to an owner’s Needs you.
5. **Device coverage**: each device reports which machines it is signed in to, so other
   devices can show “On 3 of 5 machines” and the giving device can show live progress.
6. **Recovery via `herder claim`** for an existing owner: it signs you in and hands over the
   machine’s known peers.
7. **Merge people**: move one person’s devices and roles to another on every machine.
