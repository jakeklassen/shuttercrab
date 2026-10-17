#!/bin/sh
# Empties target/ once it has grown past TARGET_BUDGET_GIB (set in
# mise.toml): Cargo never removes a build's old outputs, and each version
# bump or lockfile change leaves a whole generation of them behind.
#
#   mise run trim
set -eu
cd "$(dirname "$0")/.."

[ -d target ] || exit 0
kib=$(du -sk target | cut -f1)
if [ "$kib" -gt $((TARGET_BUDGET_GIB * 1024 * 1024)) ]; then
  echo "target/ is $((kib / 1024 / 1024)) GiB, past $TARGET_BUDGET_GIB GiB: cleaning it"
  cargo clean
fi
