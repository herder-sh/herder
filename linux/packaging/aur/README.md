# AUR package

`herder-gtk` on the [AUR](https://aur.archlinux.org/packages/herder-gtk) builds the Linux app
from a release tag's source tarball and installs `/usr/bin/herder-gtk`, its desktop entry and
its icon (and its AppStream metadata, once a tag ships `linux/data/sh.herder.Herder.metainfo.xml`).

`PKGBUILD` and `.SRCINFO` here are the source of truth. On each `v*` tag (not prereleases) the
`aur` job of `.github/workflows/release.yml`, after the GitHub Release is published, sets
`pkgver` to the tag and `pkgrel=1`, updates the tarball's checksum, regenerates `.SRCINFO` in
an Arch container and pushes both to `ssh://aur@aur.archlinux.org/herder-gtk.git`. Without the
`AUR_SSH_PRIVATE_KEY` secret (forks, or before the setup below) the job only leaves a notice.
The copies here keep the version they were last edited at; change them here, not on the AUR.

## Install

```sh
yay -S herder-gtk        # or paru -S herder-gtk
```

On Omarchy: menu → Install → AUR → `herder-gtk`. Then open herder from the launcher, or run
`herder-gtk` (`herder-gtk --demo` opens the demo fleet).

## Build it locally

```sh
cd linux/packaging/aur
makepkg -si              # builds, runs the tests under the broadway backend, installs
namcap PKGBUILD herder-gtk-*.pkg.tar.zst
```

To try an untagged checkout, point `source` at a local tarball
(`git archive --prefix=herder-$pkgver/ -o herder-$pkgver.tar.gz HEAD`) and run `updpkgsums`;
don't commit that.

## One-time setup (owner)

1. Create an account at <https://aur.archlinux.org/register> (the package's maintainer).
2. Make a key for publishing only, without a passphrase:
   `ssh-keygen -t ed25519 -C herder-aur -f ~/.ssh/aur_herder -N ''`.
   Add the public half (`~/.ssh/aur_herder.pub`) under My Account → SSH Public Key.
3. Create the package with a first push from an Arch machine (pushing to an AUR repo that
   doesn't exist yet creates it):

   ```sh
   cat >> ~/.ssh/config <<'EOF'
   Host aur.archlinux.org
     IdentityFile ~/.ssh/aur_herder
     User aur
   EOF
   git -c init.defaultBranch=master clone ssh://aur@aur.archlinux.org/herder-gtk.git /tmp/herder-gtk-aur
   cp linux/packaging/aur/PKGBUILD linux/packaging/aur/.SRCINFO /tmp/herder-gtk-aur/
   cd /tmp/herder-gtk-aur
   git add PKGBUILD .SRCINFO
   git commit -m "Add herder-gtk 0.25.0"
   git push origin master
   ```

   The AUR only takes the `master` branch. Check
   <https://aur.archlinux.org/packages/herder-gtk> shows it. Optionally edit the
   `# Maintainer:` line in `PKGBUILD` to your name and email first, here and in the push.
4. Give the release workflow the key: in github.com/herder-sh/herder → Settings → Secrets and
   variables → Actions → New repository secret, name `AUR_SSH_PRIVATE_KEY`, value the whole
   private key file (`cat ~/.ssh/aur_herder`). Or:
   `gh secret set AUR_SSH_PRIVATE_KEY -R herder-sh/herder < ~/.ssh/aur_herder`.

From the next tag on, the release updates the package; check the `update the AUR package`
job and the AUR page after it.
