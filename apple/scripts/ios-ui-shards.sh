#!/usr/bin/env bash
# Splits the iOS UI tests (apple/UITests) into the shards the apple workflow runs side by side,
# and prints them as the workflow's matrix:
#
#   apple/scripts/ios-ui-shards.sh
#   [{"shard":"1","only":"-only-testing:herder-iOS-UITests/FakeFleetUITests ..."}, ...]
#
# Fails when a test class is in no shard, in two, or a shard names a class that does not exist,
# so a new test class cannot go unrun: add it to the lightest shard. The shards are balanced by
# each class's time in a CI run (seconds, in the comments); a new shard is a new line.
set -euo pipefail

shards=(
  # 203 s
  "FakeFleetUITests TaskRowUITests SessionKeyboardUITests DemoUITests TranscriptScrollUITests"
  # 201 s
  "SkillMentionUITests PhotoAttachUITests PasskeyApprovalUITests TranscriptInlineCodeUITests TabsUITests"
  # 202 s
  "SecretsUITests TranscriptCopyUITests SessionLinkUITests SessionSearchUITests WaitingTaskUITests"
)

cd "$(dirname "$0")/../UITests"
classes=$(sed -nE 's/^[[:space:]]*(final[[:space:]]+)?class[[:space:]]+([A-Za-z0-9_]+)[[:space:]]*:[[:space:]]*XCTestCase.*/\2/p' ./*.swift | sort)
sharded=$(printf '%s\n' "${shards[@]}" | tr ' ' '\n' | sed '/^$/d' | sort)

status=0
for class in $(comm -23 <(echo "$classes") <(echo "$sharded" | uniq)); do
  echo "$class is in no shard: add it to one in $0" >&2
  status=1
done
for class in $(comm -13 <(echo "$classes") <(echo "$sharded" | uniq)); do
  echo "$class is in a shard but is no test class in apple/UITests" >&2
  status=1
done
for class in $(echo "$sharded" | uniq -d); do
  echo "$class is in more than one shard" >&2
  status=1
done
[ "$status" -eq 0 ] || exit "$status"

for i in "${!shards[@]}"; do
  only=$(printf -- '-only-testing:herder-iOS-UITests/%s ' ${shards[$i]})
  printf '{"shard":"%d","only":"%s"}\n' "$((i + 1))" "${only% }"
done | jq -cs .
