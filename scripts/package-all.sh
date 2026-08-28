#!/usr/bin/env bash
# Package every plugin at its manifest version via scripts/package.sh,
# signed with the maintainer key from VYNKOR_SIGNING_KEY_FILE.
# Reads /tmp/opencode/package_list.tsv: dir<TAB>slug<TAB>name<TAB>desc<TAB>cat<TAB>tags<TAB>status
set -euo pipefail
KEY="${VYNKOR_SIGNING_KEY_FILE:?set VYNKOR_SIGNING_KEY_FILE}"
REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

fail=0
while IFS=$'\t' read -r dir slug name desc cat tags status; do
    echo "════ packaging $dir (slug=$slug) ════"
    args=("$dir" "$name" "$desc" --category "$cat" --status "$status")
    [[ -n "$tags" ]] && args+=(--tags "$tags")
    if ! VYNKOR_SIGNING_KEY_FILE="$KEY" "$REPO/scripts/package.sh" "${args[@]}"; then
        echo "!!! package.sh FAILED for $dir"
        fail=1
    fi
done < /tmp/opencode/package_list.tsv
exit $fail
