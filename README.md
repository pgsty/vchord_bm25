# vchord_bm25

[![Website: vchord.pgsty.com](https://img.shields.io/badge/Website-vchord.pgsty.com-slategray?style=flat&logo=cilium&logoColor=white)](https://vchord.pgsty.com/)
[![Docs: bm25](https://img.shields.io/badge/Docs-bm25-slategray?style=flat)](https://vchord.pgsty.com/docs/bm25/)
[![Catalog: bm25](https://img.shields.io/badge/Catalog-bm25-%233E668F?style=flat&logo=postgresql&logoColor=white&labelColor=3E668F)](https://pgext.cloud/ext/vchord_bm25)
[![Version: v1.0.0](https://img.shields.io/badge/Version-v1.0.0-slategray?style=flat)](https://github.com/pgsty/vchord_bm25/releases)
[![License: AGPL-3.0](https://img.shields.io/badge/License-AGPL--3.0-slategray?style=flat&logo=gnu&logoColor=white)](https://github.com/pgsty/vchord_bm25/blob/main/LICENSE)
[![GitHub Stars](https://img.shields.io/github/stars/pgsty/vchord_bm25?style=flat&logo=github&logoColor=black&color=slategray)](https://star-history.com/#pgsty/vchord_bm25&Date)
[![PostgreSQL: 14-19](https://img.shields.io/badge/PostgreSQL-14--19-%233E668F?style=flat&logo=postgresql&logoColor=white&labelColor=3E668F)](https://pgext.cloud/ext/vchord_bm25)

**`vchord_bm25`** is a PostgreSQL extension for native BM25 full-text ranking powered by the Block-WeakAnd (WAND) algorithm, designed to work alongside [`pg_tokenizer`](https://github.com/pgsty/pg_tokenizer) and [`vchord`](https://github.com/pgsty/vchord).

[Website](https://vchord.pgsty.com/) | [Documentation](https://vchord.pgsty.com/docs/bm25/) | [vchord](https://github.com/pgsty/vchord) | [pg_tokenizer](https://github.com/pgsty/pg_tokenizer) | [Blog](https://vchord.pgsty.com/blog/) | [PGEXT](https://pgext.cloud/ext/vchord_bm25) | [Releases](https://github.com/pgsty/vchord_bm25/releases) | [Issues](https://github.com/pgsty/vchord_bm25/issues) | [Upstream](https://github.com/supervc-stack/VectorChord-bm25)

> [!IMPORTANT]
> **1.0.0** is the first PGSTY release. It indexes PostgreSQL's native `tsvector` and uses a new index format; it is not compatible with the `bm25vector` API or indexes of upstream 0.3.0. See [Migration from 0.3.0](#migration-from-030).

---

## Vchord Suite

`vchord_bm25` is part of the **Vchord** extension family maintained by [PGSTY](https://pgsty.com):

- **[`vchord`](https://github.com/pgsty/vchord)** ([Docs](https://vchord.pgsty.com/docs/vchord/) / [Meta](https://pgext.cloud/ext/vchord)): Scalable, high-performance, and disk-friendly vector similarity search.
- **[`vchord_bm25`](https://github.com/pgsty/vchord_bm25)** ([Docs](https://vchord.pgsty.com/docs/bm25/) / [Meta](https://pgext.cloud/ext/vchord_bm25)): Native BM25 full-text ranking index (`Block-WeakAnd`).
- **[`pg_tokenizer`](https://github.com/pgsty/pg_tokenizer)** ([Docs](https://vchord.pgsty.com/docs/tokenizer/) / [Meta](https://pgext.cloud/ext/pg_tokenizer)): Standalone multilingual text analyzer and vocabulary tokenizer.

---

## Quick start from source

Build with Rust and the development headers for your PostgreSQL installation (14–18). The `pg19` feature is available for PostgreSQL 19 builds; it is not yet part of the release test matrix.

```bash
git clone https://github.com/pgsty/vchord_bm25.git
cd vchord_bm25
PG_CONFIG=/path/to/pg_config make build
PG_CONFIG=/path/to/pg_config make install
```

Add `vchord_bm25` to `shared_preload_libraries` in `postgresql.conf`, preserving other required libraries, and restart PostgreSQL:

```ini
shared_preload_libraries = 'vchord_bm25'
```

Create the extension and an index on a stored `tsvector`. Standard PostgreSQL text search configurations work without `pg_tokenizer`:

```sql
CREATE EXTENSION vchord_bm25;

CREATE TABLE documents (
    id integer GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    passage text NOT NULL,
    vector tsvector GENERATED ALWAYS AS (to_tsvector('english', passage)) STORED
);

INSERT INTO documents (passage) VALUES
    ('PostgreSQL provides full text search.'),
    ('BM25 ranks documents by relevance.'),
    ('PostgreSQL supports BM25 ranking with vchord_bm25.');

CREATE INDEX documents_bm25 ON documents USING bm25 (vector bm25_ops)
WITH (limit = 100);

SELECT id, passage,
       vector <&> to_bm25query(to_tsvector('english', 'PostgreSQL'), 'documents_bm25') AS rank
FROM documents
ORDER BY rank
LIMIT 10;
```

`<&>` returns a **negative** BM25 score as `double precision`; ascending order puts the most relevant documents first. The query's index must match the index used by the scan. Use the same text search configuration for documents and queries. Term frequencies are retained position counts; weights A–D are ignored and document lengths use 8-bit quantization. PostgreSQL limits tsvectors to less than 1 MB, positions to 16383 and roughly 256 positions per lexeme; long documents should be split into smaller chunks ([PostgreSQL limits](https://www.postgresql.org/docs/18/textsearch-limitations.html)). Document vectors must retain positions: `strip(tsvector)` and `array_to_tsvector` do not provide term frequencies and cannot be used as indexed documents.

An expression index also works:

```sql
CREATE INDEX documents_expression_bm25
ON documents USING bm25 ((to_tsvector('english', passage)) bm25_ops)
WITH (limit = 100);
```

The query must then use that same expression and target `documents_expression_bm25`.

### Tokenizing with pg_tokenizer

[`pg_tokenizer`](https://github.com/pgsty/pg_tokenizer) 1.0.0 produces positioned `tsvector` values that `bm25_ops` indexes directly. `tokenizer_catalog.analyze_to_tsvector(text, analyzer)` uses a text analyzer's tokens as lexemes (for example Jieba for Chinese, or custom stemmer and filter pipelines); `tokenizer_catalog.tokenize_to_tsvector(text, tokenizer)` uses a model's token IDs. Both are `STABLE` because their definitions live in catalogs, so store the result in a column rather than a generated column or expression index, and build queries with the same function. Add `pg_tokenizer.so` to `shared_preload_libraries` as well.

```sql
CREATE EXTENSION pg_tokenizer;
SELECT tokenizer_catalog.create_text_analyzer('words', $$
pre_tokenizer = "unicode_segmentation"
[[character_filters]]
to_lowercase = {}
$$);

CREATE TABLE articles (id integer PRIMARY KEY, passage text NOT NULL, terms tsvector);
INSERT INTO articles VALUES
    (1, 'Cat dog cat', tokenizer_catalog.analyze_to_tsvector('Cat dog cat', 'words')),
    (2, 'Dog and bird', tokenizer_catalog.analyze_to_tsvector('Dog and bird', 'words'));
CREATE INDEX articles_bm25 ON articles USING bm25 (terms bm25_ops);

SELECT id, terms <&> to_bm25query(tokenizer_catalog.analyze_to_tsvector('cat', 'words'), 'articles_bm25') AS rank
FROM articles
ORDER BY rank
LIMIT 10;
```

## Search parameters

| Parameter | Default | Meaning |
| --- | --- | --- |
| `bm25.enable_scan` | `true` | Allow BM25 index scans in the planner. |
| `bm25.limit` | `100` | Initial scan batch size, range `0..65535`; zero selects 100. An explicitly set value overrides index `limit`. |
| `bm25.prefilter` | `false` | Apply visibility and query filters during ranking. An explicitly set value overrides the index's `prefilter`, including `false`. |

If a GUC has its default source (`RESET` or `SET ... TO DEFAULT`), the corresponding index reloption is used. Both batch defaults are 100. Explicit zero selects an automatic batch of 100. SQL `LIMIT` controls the result count independently.

```sql
SET "bm25.limit" = 100;
SET bm25.prefilter = on;
-- Return to the index's defaults:
RESET "bm25.limit";
RESET bm25.prefilter;
```

Settings from `postgresql.conf`, `ALTER DATABASE`, `ALTER ROLE`, or client startup options also override index reloptions. `RESET` restores that configured default; it falls back to the index only when the GUC source returns to the built-in default.

Scans continue in growing batches when PostgreSQL requests more rows. `limit` affects performance, not completeness: SQL `LIMIT`, `OFFSET`, and post-filtering do not silently exhaust candidates. Scores of nonmatching documents are zero; SQL NULLs sort last in ascending order. An empty query returns zero for every non-NULL document. Add a `WHERE vector @@ ...` predicate if only matching documents are wanted. Volatile predicates run in the executor, even when prefiltering is enabled.

Rows inserted after index creation are searchable immediately. Scans and scalar scoring share one statistics preparation per executor execution, index, and query; each role is permission-checked separately. Prepared statistics of one execution are bounded by `work_mem`, and an evicted query is prepared again when used. PL/pgSQL expressions evaluated outside a query, such as a loop in a `DO` block, prepare statistics on every call. Statistics count physical non-NULL index records, **including tombstones until compaction removes them**; they are not an MVCC snapshot of the corpus. NULLs are indexed for complete ordering but do not affect corpus statistics. Deletes immediately stop returning tombstoned candidates; compaction can change scores. WAND remains enabled and scales its upper bounds to account for changed average document length. Scalar scoring is `PARALLEL RESTRICTED` to keep the statistics within one executor; parallel scalar scoring is not provided.

The planner charges sequential page work and PostgreSQL CPU/cache costs, with scalar scoring declared `COST 100`. This remains a conservative heuristic, without per-term planner statistics. Small selective B-tree scans can still win. Known query index OIDs exclude other BM25 indexes. Generic plans with a runtime index OID use scalar sorting; a constant index OID permits native scans, even with parameterized query text.

## Maintenance

`VACUUM`, including parallel VACUUM, marks dead records and can merge segments while ordinary reads and writes continue. Maintenance uses the table's normal `ShareUpdateExclusiveLock`, short page locks, and atomic publication. `ANALYZE` alone never rebuilds. Old pages remain until PostgreSQL's oldest-snapshot horizon permits recycling; interrupted output is reclaimed and the free-space map repaired by a subsequent maintenance pass. WAL also publishes an appended page and its link atomically.

Automatic merging occurs when growing records reach `max(maintenance_min_rows, maintenance_scale_factor × sealed records)`, or tombstones reach `maintenance_scale_factor × total records`. Defaults are **1000 rows** and **0.1**; no-change passes do not rewrite the segment. Both are index reloptions; `maintenance_min_rows` accepts `0..2147483647`, and `maintenance_scale_factor` accepts `0..1`.

```sql
ALTER INDEX documents_bm25 SET (maintenance_min_rows = 1000, maintenance_scale_factor = 0.1);
VACUUM documents;
SELECT bm25_maintain('documents_bm25');
```

`bm25_maintain(regclass)` bypasses these thresholds, returning true if it publishes a rebuilt segment, false if there is no work or an older generation still prevents rebuilding. It requires table ownership (or `MAINTAIN` privilege on PostgreSQL 17+). It does not discover dead heap tuples; run `VACUUM` for that. It holds the table's `ShareUpdateExclusiveLock` while maintenance runs.

There is one retirement slot. A long-running snapshot can delay recycling and the next merge, allowing the growing segment to grow; 1.0.0 does **not** guarantee a hard size bound or a fixed physical file size multiplier. In a completely idle database the transaction horizon may not advance either. Tune PostgreSQL's table-level `autovacuum_vacuum_insert_threshold` and `autovacuum_vacuum_insert_scale_factor` for insert-heavy workloads, and avoid long transactions. Reclaimed pages are reused, not necessarily truncated from the file. Standby readers that meet pages recycled on the primary fail with SQLSTATE `40001`; retry them or use `hot_standby_feedback`, accepting that it may delay primary-side reclamation. Tombstones themselves never cancel standby queries: PostgreSQL resolves those conflicts when it replays the heap pruning that precedes them.

Index reloptions also accept a TOML `options` string for `k1` (default `1.2`, range `1.2..2.0`) and `b` (default `0.75`, range `0..1`):

```sql
ALTER INDEX documents_bm25 SET (limit = 100, prefilter = true);
```

## SQL API and schema permissions

- `bm25query` is `(vector tsvector, index regclass)`.
- `to_bm25query(tsvector, regclass)` constructs a query, in that argument order.
- `tsvector <&> bm25query` returns `double precision`.
- `bm25_maintain(regclass)` requests segment maintenance and returns `boolean`.
- `bm25_ops` is the explicit operator class for `tsvector` with `USING bm25`.
- `bm25vector`, its integer-array casts, and the old `bm25_catalog.*` GUCs are not part of 1.0.0.

The extension is relocatable. It installs in the selected schema (normally `public`), with no fixed `bm25_catalog`. A DBA can choose a dedicated schema and grant access:

```sql
CREATE SCHEMA search;
CREATE EXTENSION vchord_bm25 WITH SCHEMA search;
GRANT USAGE ON SCHEMA search TO app_user;
ALTER ROLE app_user SET search_path = public, search;
```

Use this as an alternative to the quick-start installation, not a second installation in the same database. PostgreSQL grants function execution to `PUBLIC` by default; deployments with restricted default privileges must also grant the required function privileges. Table permissions remain separate. Schema-qualified calls to `search.to_bm25query(...)` work even when `search` is absent from `search_path`, including after `ALTER EXTENSION ... SET SCHEMA`.

Scoring and native index scans require `SELECT` on the indexed table. For an ordinary, non-partial index on a single column, `SELECT` on that column is sufficient. Expression and partial indexes require table-level `SELECT`. The check uses the current role even when scoring a caller-supplied vector without a `FROM` clause; schema, function, or view access alone does not grant access to corpus statistics.

BM25 scores use statistics for the entire index. If row-level security applies to the current role, both scalar scoring and native scans are rejected rather than exposing statistics for hidden rows. PostgreSQL's owner and `BYPASSRLS` exemptions still apply; `FORCE ROW LEVEL SECURITY` subjects an ordinary owner to the restriction. Setting `row_security = off` does not bypass this check.

## Migration from 0.3.0

1.0.0 changes both the SQL API and index storage. **An in-place `ALTER EXTENSION UPDATE` from 0.3.0 is deliberately rejected**, with migration instructions, before changing any objects. Historical install/upgrade scripts remain in Git for their corresponding old binaries; they are not a compatible install schema for the new library.

1. Keep the old binary and database available while exporting source text and application data. Inventory `bm25vector` columns, indexes, views, functions, and tokenizer triggers. Export text while the old datatype's input/output functions are still installed.
2. Install the new binary in a separate PostgreSQL installation with its own library directory, or a separate container image, and create a new database with the 1.0.0 extension. A separate data directory or port using the same library directory is not sufficient. All databases in one PostgreSQL installation share its extension library, so replacing the library underneath existing 0.3.0 databases is unsafe.
3. Restore source text and generate new `tsvector` values with the desired text search configuration. Rebuild every BM25 index. Token IDs alone cannot reconstruct the original lexemes; there is no automatic `bm25vector` cast.
4. Update queries to `to_bm25query(vector, index)` and `bm25.*` settings. Update or replace tokenizer triggers. Compare search quality and results before switching the application.

`pg_tokenizer` integrations must supply `tsvector` values with positions. The historical `tokenize(...)::bm25vector` workflow only applies to 0.3.0. With pg_tokenizer 1.0.0, replace it with `tokenizer_catalog.tokenize_to_tsvector(text, tokenizer)` (the same model, with token IDs as lexemes) or `tokenizer_catalog.analyze_to_tsvector(text, analyzer)`, store the result in a `tsvector` column, and rebuild the index. Term frequencies now come from tsvector positions, so compare rankings before switching.

## Development and schema validation

```bash
PG_CONFIG=/path/to/pg_config PROFILE=dev make build
PG_CONFIG=/path/to/pg_config make clippy
cargo fmt --check -- --config-path /dev/null --config imports_granularity=Module
cargo test --locked --workspace --exclude vchord_bm25 --no-fail-fast
sqllogictest --db "$USER" --user "$USER" './tests/sqllogictest/*.slt'
./tests/concurrent_vacuum "user=$USER dbname=$USER host=localhost"
```

SQLLogicTest requires a disposable database with the extension installed and preloaded, and an administrator connection for its role and RLS fixtures. It covers ranking, filtering, growing segments, serial and parallel VACUUM, planner costs, scan errors/timeouts and cleanup, schema relocation, and table/column permissions. The nightly Rust `concurrent_vacuum` script checks reader snapshots across concurrent inserts and maintenance; it accepts the same connection-string format as `tests/fuzz`.

`tests/fuzz CONNECTION THREADS [ITERATIONS]` defaults to 5000 operations per thread. Concurrent scans check ordering, uniqueness and result counts. Exact index/scalar comparisons pause mutations because physical statistics can change between separate SQL executions, even within a repeatable-read transaction.

`xtask` extracts SQL from the built library. Release packaging checks it against `sql/install/vchord_bm25--<version>.sql` and fails on drift. To regenerate after an API edit, use `cargo run -p xtask -- schema /path/to/compiled/library > /tmp/install.sql`, review it, copy it into the intended release install file, and build again. Do not change a release version to match historical SQL from an incompatible engine.

The PG18+ acceptance harness creates its own disposable primary and standby, and requires Python with `psycopg[binary]` plus PostgreSQL's `pageinspect` extension:

```bash
python tests/acceptance.py --pg-config /path/to/pg_config --slt --recovery
```

It checks an independent BM25 formula, complete ordering, deferred retirement, real parallel workers, buffer work, insertion scaling, cancellation and crash/replay recovery. `--recovery` deliberately kills only backends in its own temporary cluster. It does not connect to your existing databases. ResourceOwner ownership is hardened; no failing-before reproducer for historical issue #1 has been established.

## Documentation

See the [BM25 docs](https://vchord.pgsty.com/docs/bm25/), [SQL reference](https://vchord.pgsty.com/docs/bm25/reference/sql/), [GUC reference](https://vchord.pgsty.com/docs/bm25/reference/gucs/), [migration guide](https://vchord.pgsty.com/docs/bm25/reference/migration/), and [tokenizer docs](https://vchord.pgsty.com/docs/tokenizer/). Historical 0.3.0 examples remain in the version-labeled site documentation.

## License

This software is licensed under the **GNU Affero General Public License v3.0 (AGPLv3)**. You may use, modify, and distribute it under the terms of the [AGPLv3](LICENSE).

- Copyright (c) 2025-2026 TensorChord Inc.
- Copyright (c) 2026 PGSTY PTE. LTD.
- Maintained by [Ruohang Feng](mailto:rh@vonng.com) @ [PGSTY](https://pgsty.com)
