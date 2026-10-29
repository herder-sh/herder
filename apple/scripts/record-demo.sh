#!/usr/bin/env bash
# Records the demo fleet (`fake_daemon --demo`) into the demo the apps open from their empty
# state ("Explore a demo"): apple/HerderKit/Sources/HerderKit/Resources/demo.json. The release
# workflow runs it before each build, so a release shows a fleet recorded the same day; run it
# and commit the file when the demo fleet changes.
set -euo pipefail

cd "$(dirname "$0")/../.."
cargo build -p herder-ffi --example fake_daemon
"${CARGO_TARGET_DIR:-target}/debug/examples/fake_daemon" --record-demo \
  apple/HerderKit/Sources/HerderKit/Resources/demo.json
