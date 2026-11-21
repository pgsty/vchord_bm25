#!/usr/bin/env bash
# Check that upgrading vchord_bm25 from an earlier 1.x release preserves
# existing BM25 indexes and produces the exact same extension members as a
# fresh install of the current version.
#
# Usage:
#   devtools/check-upgrade.sh FROM_VERSION

set -euo pipefail

if [ $# -ne 1 ]; then
    sed -n '2,7p' "$0" | sed 's/^# \{0,1\}//'
    exit 2
fi

from=$1
to=$(sed -n "s/^default_version = '\(.*\)'$/\1/p" "$(dirname "$0")/../vchord_bm25.control")
upgraded=vchord_bm25_check_upgrade_$$
fresh=vchord_bm25_check_fresh_$$

members="
SELECT pg_describe_object(d.classid, d.objid, d.objsubid)
    || coalesce(E'\n' || CASE
        WHEN d.classid = 'pg_proc'::regclass
            AND (SELECT prokind FROM pg_proc WHERE oid = d.objid) IN ('f', 'p')
            THEN pg_get_functiondef(d.objid)
        WHEN d.classid = 'pg_class'::regclass
            AND (SELECT relkind FROM pg_class WHERE oid = d.objid) = 'v'
            THEN pg_get_viewdef(d.objid)
    END, '')
FROM pg_depend d
WHERE d.refclassid = 'pg_extension'::regclass
    AND d.refobjid = (SELECT oid FROM pg_extension WHERE extname = 'vchord_bm25')
    AND d.deptype = 'e'
ORDER BY 1;
"

cleanup() {
    dropdb --if-exists "$upgraded" >/dev/null 2>&1 || true
    dropdb --if-exists "$fresh" >/dev/null 2>&1 || true
}
trap cleanup EXIT

createdb "$upgraded"
createdb "$fresh"

psql -X -q -v ON_ERROR_STOP=1 -d "$upgraded" <<SQL
CREATE EXTENSION vchord_bm25 VERSION '$from';
CREATE TABLE upgrade_docs (id int PRIMARY KEY, v tsvector);
INSERT INTO upgrade_docs
SELECT i, to_tsvector('simple', CASE WHEN i = 1 THEN 'target target rare' ELSE 'target filler' || i END)
FROM generate_series(1, 200) AS i;
CREATE INDEX upgrade_docs_bm25 ON upgrade_docs USING bm25 (v bm25_ops);
INSERT INTO upgrade_docs VALUES (201, to_tsvector('simple', 'target growing'));

ALTER EXTENSION vchord_bm25 UPDATE TO '$to';

DO \$\$
DECLARE
    ver text;
    top_id int;
    cnt bigint;
BEGIN
    SELECT extversion INTO ver FROM pg_extension WHERE extname = 'vchord_bm25';
    IF ver IS DISTINCT FROM '$to' THEN
        RAISE EXCEPTION 'expected extversion $to, got %', ver;
    END IF;
    PERFORM set_config('enable_seqscan', 'off', true);
    PERFORM set_config('enable_sort', 'off', true);
    SELECT id INTO top_id
    FROM upgrade_docs
    ORDER BY v <&> to_bm25query(to_tsvector('simple', 'target rare'), 'upgrade_docs_bm25')
    LIMIT 1;
    IF top_id IS DISTINCT FROM 1 THEN
        RAISE EXCEPTION 'expected top id 1 after upgrade, got %', top_id;
    END IF;
    PERFORM bm25_maintain('upgrade_docs_bm25');
    SELECT count(*) INTO cnt FROM (
        SELECT id FROM upgrade_docs
        ORDER BY v <&> to_bm25query(to_tsvector('simple', 'target'), 'upgrade_docs_bm25')
    ) AS s;
    IF cnt <> 201 THEN
        RAISE EXCEPTION 'expected 201 indexed rows after upgrade maintenance, got %', cnt;
    END IF;
END
\$\$;

DROP TABLE upgrade_docs;
SQL

psql -X -q -v ON_ERROR_STOP=1 -d "$fresh" -c "CREATE EXTENSION vchord_bm25"

a=$(mktemp)
b=$(mktemp)
trap 'rm -f "$a" "$b"; cleanup' EXIT
psql -X -At -v ON_ERROR_STOP=1 -d "$upgraded" -c "$members" > "$a"
psql -X -At -v ON_ERROR_STOP=1 -d "$fresh" -c "$members" > "$b"
if ! diff -u "$b" "$a"; then
    echo "error: upgrading $from -> $to differs from a fresh install of $to (diff above: - fresh, + upgraded)" >&2
    exit 1
fi
echo "ok: upgrading $from -> $to preserves existing indexes and matches a fresh install ($(grep -c '' "$a") lines of members)"
