#!/usr/bin/env bash
# Check that every C symbol referenced by supported (1.0.0+) SQL scripts is
# exported by the built library, together with its `pg_finfo_<symbol>` record.
#
# Historical 0.1.x–0.3.0 scripts belong to the discontinued bm25vector engine,
# whose 0.3.0->1.0.0 upgrade script intentionally refuses in-place upgrades.
#
# Usage:
#   devtools/check-symbols.sh LIBRARY [SQL_FILE...]

set -euo pipefail

if [ $# -lt 1 ]; then
    sed -n '2,9p' "$0" | sed 's/^# \{0,1\}//'
    exit 2
fi

lib=$1
shift
if [ ! -f "$lib" ]; then
    echo "error: library not found: $lib" >&2
    exit 2
fi

root=$(cd "$(dirname "$0")/.." && pwd)
if [ $# -eq 0 ]; then
    set -- "$root"/sql/install/vchord_bm25--[1-9]*.sql "$root"/sql/upgrade/vchord_bm25--[1-9]*.sql
fi

tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT

grep -ho "'MODULE_PATHNAME'[[:space:]]*,[[:space:]]*'[A-Za-z0-9_]*'" "$@" \
    | sed -E "s/.*'([A-Za-z0-9_]+)'$/\1/" \
    | LC_ALL=C sort -u >"$tmp/referenced"

case "$(uname -s)" in
Darwin)
    # Mach-O prefixes C symbols with an extra underscore.
    nm -gU "$lib" | awk 'NF >= 3 { print $NF }' | sed 's/^_//' ;;
*)
    nm -D --defined-only "$lib" | awk 'NF >= 3 { print $NF }' ;;
esac | LC_ALL=C sort -u >"$tmp/exported"

missing=0
total=0
while IFS= read -r sym; do
    total=$((total + 1))
    for name in "$sym" "pg_finfo_$sym"; do
        if ! grep -qxF "$name" "$tmp/exported"; then
            echo "missing: $name"
            missing=$((missing + 1))
        fi
    done
done <"$tmp/referenced"

echo "checked $total referenced symbols in $# SQL files against $lib: $missing missing"
[ "$missing" -eq 0 ]
