#!/usr/bin/env bash
set -eo pipefail

mkdir -p $(dirname "$0")/../target/tools

if [ $# -eq 0 ] ; then
    echo "Usage: $0 INITIAL_VERSION VERSIONS.."
    echo "Dump the extension members. Install INITIAL_VERSION first and upgrade to every version in VERSIONS."
    echo "The extension members are installed in database \"vchord_bm25\", so use \"createdb vchord_bm25\" to setup."
    echo "Examples:"
    echo "  ./tools/dump.sh 0.1.11 0.2.0 > ./dump_upgrade.sql"
    echo "  ./tools/dump.sh 0.2.0 > ./dump_install.sql"
    echo "  diff ./dump_upgrade.sql ./dump_install.sql"
    exit 0
fi

f=()
prev_arg=""
for arg in "$@"; do
    if [ "$prev_arg" = "" ]; then
        x=$(realpath "$(dirname "$0")/../sql/install/vchord_bm25--${arg}.sql")
    else
        x=$(realpath "$(dirname "$0")/../sql/upgrade/vchord_bm25--${prev_arg}--${arg}.sql")
    fi
    prev_arg=$arg
    f+=("$x")
done

so=$(cd "$(dirname "$0")/../target/tools" && pwd)/vchord_bm25.so
cc_args=($(pg_config --cppflags))
if [ "$(uname -s)" = "Darwin" ]; then
    cc_args+=(-Wl,-undefined,dynamic_lookup)
fi
$(dirname "$0")/dump-codegen.sh | gcc "${cc_args[@]}" -I "$(pg_config --includedir-server)" -fPIC -shared -o "$so" -x c -

sql=$(mktemp)
trap 'rm -f "$sql"' EXIT
echo "BEGIN;" >> $sql
echo "CREATE SCHEMA vchord_bm25;" >> $sql
echo "SET LOCAL search_path TO vchord_bm25,public;" >> $sql
cat ${f[@]} \
    | grep -v '^\\' \
    | sed "s|@extschema@|vchord_bm25|g" \
    | sed "s|MODULE_PATHNAME|$so|g" \
    >> $sql
echo "END;" >> $sql

psql -X -v ON_ERROR_STOP=1 -d vchord_bm25 -f "$sql" 1>&2
pg_dump -s -d vchord_bm25
psql -X -v ON_ERROR_STOP=1 -d vchord_bm25 -c "DROP SCHEMA IF EXISTS vchord_bm25 CASCADE;" 1>&2
