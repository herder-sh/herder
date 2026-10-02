#!/bin/sh
# Installs herder to ~/.local/bin/herder.
#
#   curl -fsSL https://raw.githubusercontent.com/herder-sh/herder/main/install.sh | sh
#
# HERDER_VERSION=1.2.3 picks a release instead of the latest one. HERDER_DOWNLOAD_BASE
# replaces https://github.com/herder-sh/herder/releases, laid out the same way:
# <base>/latest redirects to <base>/tag/v<version>, assets live in <base>/download/v<version>/.

set -eu

say() {
	printf 'herder: %s\n' "$*"
}

fail() {
	printf 'herder: %s\n' "$*" >&2
	exit 1
}

need() {
	command -v "$1" >/dev/null 2>&1 || fail "$1 is required but not installed"
}

main() {
	need curl
	need tar
	need sha256sum
	need mktemp
	need uname

	os=$(uname -s)
	[ "$os" = Linux ] || fail "herder runs on Linux only, not $os"
	case $(uname -m) in
	x86_64 | amd64) arch=x86_64 ;;
	aarch64 | arm64) arch=aarch64 ;;
	*) fail "herder has no release build for $(uname -m)" ;;
	esac
	target="$arch-unknown-linux-musl"

	base=${HERDER_DOWNLOAD_BASE:-https://github.com/herder-sh/herder/releases}
	base=${base%/}
	if [ -n "${HERDER_VERSION:-}" ]; then
		version=${HERDER_VERSION#v}
	else
		url=$(curl -fsSLI -o /dev/null -w '%{url_effective}' "$base/latest") ||
			fail "cannot reach $base/latest"
		case $url in
		*/tag/v*) version=${url##*/tag/v} ;;
		*) fail "no herder release is published at $base" ;;
		esac
	fi

	name="herder-$version-$target"
	tmp=$(mktemp -d)
	trap 'rm -rf "$tmp"' EXIT
	say "downloading herder $version for $target"
	curl -fsSL -o "$tmp/$name.tar.gz" "$base/download/v$version/$name.tar.gz" ||
		fail "cannot download $base/download/v$version/$name.tar.gz"
	curl -fsSL -o "$tmp/$name.tar.gz.sha256" "$base/download/v$version/$name.tar.gz.sha256" ||
		fail "cannot download $base/download/v$version/$name.tar.gz.sha256"

	expected=$(cut -d ' ' -f 1 <"$tmp/$name.tar.gz.sha256")
	actual=$(sha256sum "$tmp/$name.tar.gz" | cut -d ' ' -f 1)
	[ "$expected" = "$actual" ] ||
		fail "checksum mismatch for $name.tar.gz: expected $expected, got $actual"
	tar -xzf "$tmp/$name.tar.gz" -C "$tmp"

	bin_dir="$HOME/.local/bin"
	mkdir -p "$bin_dir"
	# Copy next to the target, then rename: a running herder keeps its old binary intact.
	cp "$tmp/$name/herder" "$bin_dir/.herder-install"
	chmod 755 "$bin_dir/.herder-install"
	mv -f "$bin_dir/.herder-install" "$bin_dir/herder"
	say "installed herder $version to $bin_dir/herder"

	case ":$PATH:" in
	*":$bin_dir:"*) ;;
	*)
		say "$bin_dir is not on your PATH; add this to your shell profile:"
		# shellcheck disable=SC2016 # printed for the user to paste, not expanded here
		printf '  export PATH="$HOME/.local/bin:$PATH"\n'
		;;
	esac
	say "to run the daemon at boot: herder service install"
}

main "$@"
