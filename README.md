# vchord_bm25

[![Website: vchord.pgsty.com](https://img.shields.io/badge/Website-vchord.pgsty.com-slategray?style=flat&logo=cilium&logoColor=white)](https://vchord.pgsty.com/)
[![Docs: bm25](https://img.shields.io/badge/Docs-bm25-slategray?style=flat)](https://vchord.pgsty.com/docs/bm25/)
[![Catalog: bm25](https://img.shields.io/badge/Catalog-bm25-%233E668F?style=flat&logo=postgresql&logoColor=white&labelColor=3E668F)](https://pgext.cloud/ext/vchord_bm25)
[![Version: 1.3.0](https://img.shields.io/badge/Version-1.3.0-slategray?style=flat)](docs/release-notes/1.3.0.md)
[![License: AGPL-3.0](https://img.shields.io/badge/License-AGPL--3.0-slategray?style=flat&logo=gnu&logoColor=white)](https://github.com/pgsty/vchord_bm25/blob/main/LICENSE)
[![GitHub Stars](https://img.shields.io/github/stars/pgsty/vchord_bm25?style=flat&logo=github&logoColor=black&color=slategray)](https://star-history.com/#pgsty/vchord_bm25&Date)
[![PostgreSQL: 15-19](https://img.shields.io/badge/PostgreSQL-15--19-%233E668F?style=flat&logo=postgresql&logoColor=white&labelColor=3E668F)](#build-from-source)

vchord_bm25 1.3.0 provides native BM25 full-text ranking for PostgreSQL 15–19 on `x86_64` and `aarch64`, using Block-WeakAnd (WAND).

[Documentation](https://vchord.pgsty.com/docs/bm25/) · [Releases](https://github.com/pgsty/vchord_bm25/releases) · [Issues](https://github.com/pgsty/vchord_bm25/issues)

## Features

- Index PostgreSQL `tsvector` values with the `bm25` access method.
- Rank documents with BM25 scores, with filtering and incremental index maintenance.
- Use PostgreSQL text search configurations or multilingual pipelines from [pg_tokenizer](https://github.com/pgsty/pg_tokenizer).
- Combine with [vchord](https://github.com/pgsty/vchord) for hybrid text and vector search.

## Installation

Install from the [Pigsty package repository](https://pigsty.io/ext/e/vchord_bm25):

```bash
pig repo add pgsql -u
pig ext add vchord_bm25
```

The GitHub release workflow builds Linux `.deb` and `.zip` packages on Ubuntu 22.04 for PostgreSQL 15–19 on both architectures. The `.deb` packages require `libc6 >= 2.35`. Choose the package for your PostgreSQL major version and CPU architecture. See [GitHub Releases](https://github.com/pgsty/vchord_bm25/releases) and the [installation guide](https://vchord.pgsty.com/docs/bm25/getting-started/installation/).

Add `vchord_bm25` to `shared_preload_libraries`, restart PostgreSQL, then enable the extension:

```sql
CREATE EXTENSION vchord_bm25;
```

## Usage

```sql
CREATE TABLE documents (
    id bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    passage text NOT NULL,
    terms tsvector GENERATED ALWAYS AS (to_tsvector('english', passage)) STORED
);

INSERT INTO documents (passage) VALUES
    ('PostgreSQL provides full text search.'),
    ('BM25 ranks documents by relevance.'),
    ('PostgreSQL supports BM25 ranking with vchord_bm25.');

CREATE INDEX documents_bm25 ON documents USING bm25 (terms bm25_ops);

SELECT id, passage,
       terms <&> to_bm25query(to_tsvector('english', 'PostgreSQL'), 'documents_bm25') AS rank
FROM documents
ORDER BY rank
LIMIT 10;
```

`<&>` returns a negative BM25 score; ascending order returns the most relevant documents first. Use the same text analysis for documents and queries. Document vectors must retain positions, so do not use `strip(tsvector)` or `array_to_tsvector` for indexed documents.

For multilingual analysis, store the result of `tokenizer_catalog.analyze_to_tsvector` or `tokenize_to_tsvector` in a `tsvector` column and index it with `bm25_ops`. See [indexing](https://vchord.pgsty.com/docs/bm25/usage/indexing/) and [querying](https://vchord.pgsty.com/docs/bm25/usage/querying/).

## Tuning and Maintenance

| Setting | Default | Purpose |
| --- | --- | --- |
| `bm25.enable_scan` | `true` | Allow BM25 index scans. |
| `bm25.limit` | `100` | Initial scan batch size; SQL `LIMIT` controls the result count. |
| `bm25.prefilter` | `false` | Apply visibility and query filters during ranking. |

```sql
SET "bm25.limit" = 100;
SET bm25.prefilter = on;
VACUUM documents;
```

`VACUUM` removes dead entries and merges segments. Long transactions can delay page reuse.

When row security applies to the caller, BM25 scoring requires the index owner's opt-in to whole-index statistics. This setting defaults to off.

```sql
ALTER INDEX documents_bm25 SET (shared_corpus_stats = on);
```

Scores use document counts, per-term document frequencies and average document length across the index, so they can reveal term prevalence across tenants. PostgreSQL enforces row policies on returned rows. The caller also needs table-level `SELECT`, or column-level `SELECT` covering the index key and predicate with `EXECUTE` on the functions used in supported expressions. A view grant alone is insufficient. Run `ALTER INDEX documents_bm25 RESET (shared_corpus_stats)` to withdraw the opt-in and before switching back to an older binary.

For prepared Top-K queries, keep the corpus index fixed and parameterize the search text. This lets a generic plan identify the index.

```sql
PREPARE search_docs(text) AS
SELECT id FROM documents
ORDER BY terms <&> to_bm25query(to_tsvector('english', $1), 'documents_bm25'::regclass)
LIMIT 10;
```

The [SQL reference](https://vchord.pgsty.com/docs/bm25/reference/sql/) and [settings](https://vchord.pgsty.com/docs/bm25/reference/gucs/) cover the query and index options.

## Build from Source

Building from source requires Rust 1.100.0 or later, PostgreSQL 15–19 development headers on `x86_64` or `aarch64`, GNU Make, and matching Clang, libclang, and LLVM tools. The extension and its SQL generator use pgrx 0.20.1.

```bash
git clone --branch 1.3.0 --depth 1 https://github.com/pgsty/vchord_bm25.git
cd vchord_bm25
PG_CONFIG=/path/to/pg_config make build
sudo make PG_CONFIG=/path/to/pg_config install
```

Set `CLANG_PATH` and `LIBCLANG_PATH` if multiple LLVM versions are installed. For Clang installations that load a default configuration file, set `CLANG_NO_DEFAULT_CONFIG=1`.

## Upgrading

Install the new library and SQL files, restart PostgreSQL, then run in each database:

```sql
ALTER EXTENSION vchord_bm25 UPDATE;
```

Versions 1.0.0, 1.1.0 and 1.2.0 upgrade in place and keep their format-4 indexes. PostgreSQL 14 installations must first move to a supported server major. Legacy `bm25vector` installations require data migration and index rebuilding. See the [1.3.0 release notes](docs/release-notes/1.3.0.md) and the [migration guide](https://vchord.pgsty.com/docs/bm25/reference/migration/).

## Testing

```bash
cargo test --locked --workspace --exclude vchord_bm25
PG_CONFIG=/path/to/pg_config make clippy
sqllogictest --db "$USER" --user "$USER" './tests/sqllogictest/*.slt'
```

SQL tests require a running PostgreSQL instance with the extension installed. `tests/acceptance.py` provides additional maintenance, concurrency, and recovery checks in disposable clusters.

## License

[GNU Affero General Public License v3.0](LICENSE).

Copyright (c) 2025–2026 TensorChord Inc. and 2026 PGSTY PTE. LTD. Maintained by [Ruohang Feng](mailto:rh@vonng.com) at [PGSTY](https://pgsty.com).
