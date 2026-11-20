# vchord_bm25

[![Website: vchord.pgsty.com](https://img.shields.io/badge/Website-vchord.pgsty.com-slategray?style=flat&logo=cilium&logoColor=white)](https://vchord.pgsty.com/)
[![Docs: bm25](https://img.shields.io/badge/Docs-bm25-slategray?style=flat)](https://vchord.pgsty.com/docs/bm25/)
[![Catalog: bm25](https://img.shields.io/badge/Catalog-bm25-%233E668F?style=flat&logo=postgresql&logoColor=white&labelColor=3E668F)](https://pgext.cloud/ext/vchord_bm25)
[![Version: v1.2.0](https://img.shields.io/badge/Version-v1.2.0-slategray?style=flat)](https://github.com/pgsty/vchord_bm25/releases)
[![License: AGPL-3.0](https://img.shields.io/badge/License-AGPL--3.0-slategray?style=flat&logo=gnu&logoColor=white)](https://github.com/pgsty/vchord_bm25/blob/main/LICENSE)
[![GitHub Stars](https://img.shields.io/github/stars/pgsty/vchord_bm25?style=flat&logo=github&logoColor=black&color=slategray)](https://star-history.com/#pgsty/vchord_bm25&Date)
[![PostgreSQL: 15-19](https://img.shields.io/badge/PostgreSQL-15--19-%233E668F?style=flat&logo=postgresql&logoColor=white&labelColor=3E668F)](https://pgext.cloud/ext/vchord_bm25)

Native BM25 full-text ranking for PostgreSQL 15–19, using the Block-WeakAnd (WAND) algorithm.

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

Packages and source archives are also available from [GitHub Releases](https://github.com/pgsty/vchord_bm25/releases).

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

`VACUUM` removes dead entries and merges segments; long transactions can delay page reuse. Scoring requires access to the indexed table or column and rejects applicable row-level security. See the [SQL reference](https://vchord.pgsty.com/docs/bm25/reference/sql/) and [settings](https://vchord.pgsty.com/docs/bm25/reference/gucs/) for details.

## Build from Source

Requires Rust 1.99.1+, PostgreSQL development headers, GNU Make, and matching Clang, libclang, and LLVM tools. The project uses pgrx 0.20.0.

```bash
git clone https://github.com/pgsty/vchord_bm25.git
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

Legacy `bm25vector` installations require data migration and index rebuilding; see the [migration guide](https://vchord.pgsty.com/docs/bm25/reference/migration/).

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
