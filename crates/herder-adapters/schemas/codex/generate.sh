#!/bin/sh
# Regenerates the codex app-server schema snapshot from the installed `codex`, keeping only the
# schemas the adapter's messages are checked against. Commit the result with the new VERSION;
# then `cargo test -p herder-adapters --test codex` shows what drifted.
set -eu
dir=$(dirname "$0")
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
codex app-server generate-json-schema --out "$tmp"
codex --version >"$dir/VERSION"
for schema in \
    ClientRequest ClientNotification ServerRequest ServerNotification \
    CommandExecutionRequestApprovalResponse FileChangeRequestApprovalResponse \
    v1/InitializeResponse v2/GetAccountResponse v2/GetAccountRateLimitsResponse \
    v2/ThreadStartResponse v2/ThreadInjectItemsResponse v2/TurnStartResponse \
    v2/TurnInterruptResponse; do
    cp "$tmp/$schema.json" "$dir/"
done
