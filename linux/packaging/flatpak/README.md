# Flatpak

The Flatpak of the Linux app (`linux/`, the `herder-gtk` crate), app ID `sh.herder.Herder`, on
the GNOME 51 runtime. It builds from source with no network: `cargo-sources.json` vendors
every crate in `Cargo.lock`.

| File                                   | What it is                                               |
| -------------------------------------- | -------------------------------------------------------- |
| `sh.herder.Herder.yml`                 | The manifest; builds this checkout                       |
| `cargo-sources.json`                   | The crates, generated from `Cargo.lock`                  |
| `update-cargo-sources.sh`              | Regenerates `cargo-sources.json` (needs `uv`)            |
| `check-metainfo-release.sh`            | Checks the metainfo has the workspace version's release  |
| `lint.sh`                              | `flatpak-builder-lint` on the manifest and a built repo  |
| `ci-unlayer-flatpak.sh`                | Works around a CI container runtime-update failure       |
| `../../data/sh.herder.Herder.metainfo.xml` | The AppStream metainfo                               |

## The sandbox

- `--share=network`: the app is a network client of your machines' herder daemons.
- `--socket=wayland`, `--socket=fallback-x11`, `--share=ipc`, `--device=dri`: a GTK 4 window.
- `--filesystem=xdg-config/herder:create`: the profile it shares with the herder TUI,
  `~/.config/herder` (machines, pinned certificates, the offline cache). Nothing else of
  the home directory. In the sandbox `XDG_CONFIG_HOME` is the app's own directory, so the app
  opens `$HOST_XDG_CONFIG_HOME/herder`, else `~/.config/herder`, when `FLATPAK_ID` is set.

## Keeping it current

- **After every `Cargo.lock` change**, run `linux/packaging/flatpak/update-cargo-sources.sh`
  and commit `cargo-sources.json`. The `flatpak sources` CI job fails while it is stale.
- **Every release** adds a `<release version="X.Y.Z" date="YYYY-MM-DD">` entry at the top of
  `<releases>` in the metainfo, in the release PR. The `flatpak sources` job and the release
  workflow fail without it.

## Build and install locally

```sh
flatpak install --user flathub org.flatpak.Builder
flatpak run org.flatpak.Builder --user --install --force-clean \
  --state-dir ~/.cache/herder-flatpak/state ~/.cache/herder-flatpak/build \
  linux/packaging/flatpak/sh.herder.Herder.yml
flatpak run sh.herder.Herder --demo
```

Run it from the repository root. Keep the build and state directories out of `/tmp`, which
the builder's own sandbox does not share. To lint as Flathub does, add
`--repo=REPO --mirror-screenshots-url=https://dl.flathub.org/media --compose-url-policy=full`
(Flathub's build flags; the CI job uses them too) and run
`linux/packaging/flatpak/lint.sh REPO`, which lints the manifest and that repo.

## Releases

`release.yml` builds `herder-<version>-x86_64.flatpak` (and its `.sha256`) on every `v*` tag
and attaches it to the GitHub Release. To install one:

```sh
flatpak install --user herder-0.25.0-x86_64.flatpak   # pulls the GNOME runtime from Flathub
flatpak run sh.herder.Herder
```

## Submitting to Flathub (the owner, by hand)

Flathub builds from its own repo, `flathub/sh.herder.Herder`, which exists once the first
submission merges. Its manifest is this one with the source switched from this directory to a
release tag. Read the [Flathub submission docs](https://docs.flathub.org/docs/for-app-authors/submission)
first; the steps as of this writing:

1. **The app ID's domain.** `sh.herder.Herder` claims `herder.sh`, which you must control: the
   Flathub reviewers check it, and verification (step 7) proves it.
2. **Your GitHub account** needs two-factor authentication on: Flathub requires it for
   maintainers of an app repo.
3. **Fork [flathub/flathub](https://github.com/flathub/flathub)** with *Copy the `master`
   branch only* unchecked, then clone its `new-pr` branch and branch off it:

   ```sh
   git clone --branch=new-pr git@github.com:<you>/flathub.git
   cd flathub
   git checkout -b add-sh.herder.Herder
   ```

4. **Add the manifest and the crates of the release you submit**, say `v0.25.0`:

   ```sh
   herder=~/path/to/herder            # a checkout of herder at the tag
   git -C "$herder" checkout v0.25.0
   cp "$herder/linux/packaging/flatpak/sh.herder.Herder.yml" \
      "$herder/linux/packaging/flatpak/cargo-sources.json" .
   git -C "$herder" rev-parse v0.25.0^{commit}   # the commit for the source below
   ```

   In the copied `sh.herder.Herder.yml`, replace the `type: dir` source (with its `skip`
   list) by the tag:

   ```yaml
       sources:
         - type: git
           url: https://github.com/herder-sh/herder.git
           tag: v0.25.0
           commit: <the commit above>
           x-checker-data:
             type: git
             tag-pattern: ^v([\d.]+)$
         - cargo-sources.json
   ```

   `x-checker-data` lets Flathub's external data checker open the update PRs for later tags;
   each of those PRs also needs that tag's `cargo-sources.json` copied in.

5. **Check it before opening the PR**, from the flathub clone:

   ```sh
   flatpak run --command=flathub-build org.flatpak.Builder --install sh.herder.Herder.yml
   flatpak run --command=flatpak-builder-lint org.flatpak.Builder manifest sh.herder.Herder.yml
   flatpak run --command=flatpak-builder-lint org.flatpak.Builder repo repo
   flatpak run sh.herder.Herder --demo
   ```

6. **Open the PR** against the `new-pr` base branch (not `master`) of flathub/flathub, titled
   `Add sh.herder.Herder`, and fill in its checklist. Comment `bot, build` to get a test
   build. Answer the reviewers there; the PR merges when they approve, and Flathub creates
   `flathub/sh.herder.Herder` and invites you to it. From then on, updates are PRs to that
   repo.
7. **Verify the app** once it is published: sign in at [flathub.org](https://flathub.org)
   with GitHub, open the app in the developer portal, choose website verification for
   `herder.sh`, and serve the token it gives at
   `https://herder.sh/.well-known/org.flathub.VerifiedApps.txt` — in this repo, commit it as
   `site/.well-known/org.flathub.VerifiedApps.txt` (the `pages` workflow publishes `site/`).
   Then press Verify. The app shows the verified checkmark from then on.
