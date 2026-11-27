# snap

The Linux app (`linux/`, `herder-gtk`) as a strictly confined snap, for the Snap Store and
Ubuntu's App Center. The recipe is `snap/snapcraft.yaml` at the repo root: snapcraft builds in
a VM or container that only sees the directory it is run from, and the app needs the whole
workspace. This directory holds the launcher it runs first (`shared-profile`) and these notes.

- **Base:** core24 with the `gnome` extension (GTK 4.14 and libadwaita 1.5 from the
  `gnome-46-2404` content snap; the app needs GTK 4.12 and libadwaita 1.5).
- **Build:** the rust plugin runs `cargo install --locked --path linux` from the workspace, with
  the toolchain from `rust-toolchain.toml`.
- **Command:** `herder.herder-gtk`, from the launcher as **herder** (`sh.herder.Herder`). The
  app is not called `herder` so it does not shadow the herder command line on `PATH`.
- **Plugs:** `network`, what the gnome extension adds (`desktop`, `desktop-legacy`, `gsettings`,
  `wayland`, `x11`, `opengl`/`gpu-2404`, ...) and `herder-profile`, below. It is a network client
  only: the agents run where the herder daemon is installed.

## The shared profile

The app opens the same profile as the herder command line: `~/.config/herder` (`machines.json`,
its lock, `cache/`), so a machine added in either shows in both. In a strict snap `$HOME` and
`$XDG_CONFIG_HOME` are the snap's own (`~/snap/herder/current`), so without help the app would
keep a second, empty profile there.

The snap declares a `personal-files` plug, `herder-profile`, with write access to exactly
`$HOME/.config/herder`. While it is connected, `shared-profile` links the app's profile dir
(`~/snap/herder/current/.config/herder`) to `~/.config/herder` before the app starts; nothing
else of the user's home is opened. While it is not, the app keeps its own profile in the snap,
and works as a separate device. A profile made that way is moved aside to
`herder.unshared-<time>` when the plug gets connected.

Trade-offs:

- `personal-files` is a privileged interface: the store holds any upload that uses it for
  manual review until the snap is granted the plug (see below), and only then connects it
  automatically. Until then users connect it by hand (`sudo snap connect herder:herder-profile`).
- The grant is the whole directory, as the profile is saved by writing a temporary file next to
  `machines.json` and renaming it over. That directory also holds the daemon's `daemon.toml`
  on a machine that runs one, so the snap can read it.
- Only `~/.config/herder` is shared: a herder command line run with another `XDG_CONFIG_HOME`
  keeps a profile the snap does not see.

The alternatives were weaker: the `home` interface does not reach hidden directories such as
`~/.config`; classic confinement needs its own store approval, which the store gives only to
tools that cannot work confined, and gives up the sandbox; and a profile private to the snap
contradicts the shared profile the app promises.

## CI

- `.github/workflows/snap.yml` builds the snap for amd64 and arm64 with
  `snapcore/action-build` on PRs that touch `linux/`, `snap/`, `Cargo.toml`, `Cargo.lock` or
  `rust-toolchain.toml`, checks it holds the binary, the desktop entry and the icon, and
  uploads it as the `herder-snap-<arch>` artifact. It is not part of the required `ci` check.
- `.github/workflows/release.yml` calls it on a `v*` tag, attaches both snaps (and their
  `.sha256`) to the GitHub Release, then uploads them to the Snap Store with
  `snapcore/action-publish`: to **stable** for a release tag (`v1.2.0`), to **edge** for a
  pre-release tag (`v1.2.0-rc.1`), as the GitHub Release marks it. Without the
  `SNAPCRAFT_STORE_CREDENTIALS` secret that job only says so and succeeds.

## For the owner: publishing

Done once, by hand; nothing in CI registers names or creates accounts.

1. Create an Ubuntu One account at <https://login.ubuntu.com> (or use yours) and sign in to
   <https://snapcraft.io/account> once to accept the developer terms.
2. On a machine with snapd: `sudo snap install snapcraft --classic`, then `snapcraft login`.
3. Register the name: `snapcraft register herder`. On 2026-11-27 no snap named `herder` was
   published (`api.snapcraft.io/v2/snaps/info/herder` returned 404), but a registered,
   unpublished name looks the same, so only this step tells. If the name is taken, pick
   another (`herder-app`, also unpublished on that date), change `name:` in
   `snap/snapcraft.yaml` and the command in these notes (`<name>.herder-gtk`).
4. Export credentials limited to this snap and to uploading and releasing:

   ```sh
   snapcraft export-login --snaps=herder \
     --acls package_access,package_push,package_update,package_release \
     --expires 2027-12-31T00:00:00 herder-store-credentials
   ```

5. Add the file's contents as the repo secret `SNAPCRAFT_STORE_CREDENTIALS`
   (`gh secret set SNAPCRAFT_STORE_CREDENTIALS < herder-store-credentials`, then delete the
   file). Renew it before it expires.
6. Ask for the `personal-files` plug: post in the **store-requests** category of
   <https://forum.snapcraft.io> asking for auto-connection of `herder-profile`
   (`personal-files`, write `$HOME/.config/herder`) for the `herder` snap, with the reason
   above: the app is the GUI of the herder command line and shares its paired machines in
   `~/.config/herder`. Until it is granted, uploads stay in manual review and the release
   job's upload reports that.
7. Optional, for the store page: screenshots (`docs/screenshots/p8-8/`) and the description on
   <https://snapcraft.io/herder/listing>.

The next `v*` tag then publishes. To publish a tag's snap by hand instead: download
`herder_<version>_amd64.snap` from the GitHub Release and run
`snapcraft upload --release=stable herder_<version>_amd64.snap`.

## Installing

Once published: search **herder** in App Center, or

```sh
sudo snap install herder
sudo snap connect herder:herder-profile   # until the store connects it automatically
```

Then start **herder** from the launcher, or `herder.herder-gtk` (`herder.herder-gtk --demo`
for the demo fleet).

A snap from CI (the `herder-snap-amd64` artifact of a snap run) or a GitHub Release installs
without the store:

```sh
sudo snap install --dangerous ./herder_<version>_amd64.snap
sudo snap connect herder:herder-profile
```

## Building locally

On Ubuntu, with snapd: `sudo snap install snapcraft --classic`, then `snapcraft pack` from the
repo root (it builds in an LXD container; `snapcraft pack --use-lxd` after `sudo snap install
lxd && sudo lxd init --auto`). It builds the committed tree (`source-type: git`): commit what
you want in the snap first.
