# Development after 1.2.0

- Reduce Generic WAL records during index builds and segment rebuilds by batching posting checkpoints across terms. Parallel participants report build progress every 1,024 rows and at the end of their scans.
- Keep exact combined corpus statistics for sealed and growing documents. Scalar scoring reuses index metadata within an execution. Ranked scans retain positive growing candidates and read zero-score and NULL rows as pagination reaches them.
- Price first-batch preparation separately from repeated posting scans so PostgreSQL can use an ordered BM25 scan for selective Top-K queries.
- Let index owners enable whole-index statistics under row security with `shared_corpus_stats`. The default is off. Scoring checks the caller's corpus privileges on scalar, index and view paths. Column grants can cover simple index expressions and partial predicates. PostgreSQL applies row policies before returning rows.
- Prefilter repeatable correlated subqueries over plain heap or B-tree scans, including scalar `count(*)`. PostgreSQL evaluates volatile subqueries and security-sensitive filters.
- Delay scratch-file creation until a rebuild is needed. Remove up to three retired pages from their chain in one WAL record and publish free-space hints after the record completes.

The SQL API and format-4 indexes support in-place binary upgrades from 1.1.0 and 1.2.0. Reset `shared_corpus_stats` on indexes where it was explicitly set before returning to an earlier binary.
