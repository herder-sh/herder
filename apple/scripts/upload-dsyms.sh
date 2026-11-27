#!/usr/bin/env bash
# Uploads the dSYMs of an archive to the Sentry project the app's DSN names, so its crashes,
# hangs and profiles show herder's functions and lines. Skips, saying why, without
# SENTRY_AUTH_TOKEN, SENTRY_ORG or a DSN, as from a fork; warns rather than failing the release
# when the upload fails.
#
#   apple/scripts/upload-dsyms.sh <dsn> <archive.xcarchive>
#
# The project is found by the id the DSN ends with, in the organization SENTRY_ORG, on the
# Sentry region the DSN's host is in (de.sentry.io for the EU).
set -euo pipefail

dsn=${1:-}
archive=${2:?usage: upload-dsyms.sh <dsn> <archive.xcarchive>}
if [ -z "${SENTRY_AUTH_TOKEN:-}" ] || [ -z "${SENTRY_ORG:-}" ] || [ -z "$dsn" ]; then
  echo "no Sentry token, organization or DSN: the dSYMs are not uploaded"
  exit 0
fi

host=${dsn#*@}
host=${host%%/*}
case $host in
  *.de.sentry.io) url=https://de.sentry.io ;;
  *) url=https://sentry.io ;;
esac
project_id=${dsn##*/}

warn() {
  echo "::warning::dSYMs not uploaded to Sentry: $*"
  exit 0
}

slug=$(curl -fsS -H "Authorization: Bearer $SENTRY_AUTH_TOKEN" \
  "$url/api/0/organizations/$SENTRY_ORG/projects/" |
  jq -r --arg id "$project_id" '.[] | select(.id == $id) | .slug') ||
  warn "cannot list the projects of $SENTRY_ORG"
[ -n "$slug" ] || warn "$SENTRY_ORG has no project $project_id"

command -v sentry-cli >/dev/null || brew install getsentry/tools/sentry-cli >/dev/null ||
  warn "cannot install sentry-cli"
SENTRY_URL=$url sentry-cli debug-files upload --org "$SENTRY_ORG" --project "$slug" \
  "$archive/dSYMs" || warn "sentry-cli failed"
echo "uploaded the dSYMs to Sentry's $SENTRY_ORG/$slug"
