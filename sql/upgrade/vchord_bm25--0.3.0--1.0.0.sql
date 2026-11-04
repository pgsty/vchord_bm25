-- vchord_bm25--0.3.0--1.0.0.sql

-- complain if this file is sourced in psql, rather than via CREATE EXTENSION
\echo Use "ALTER EXTENSION vchord_bm25 UPDATE TO '1.0.0'" to load this file. \quit

-- 1.0.0 has a different SQL API and on-disk index format. Never relabel an
-- old index as compatible, drop user columns, or silently CASCADE.
DO $$
BEGIN
    RAISE EXCEPTION 'vchord_bm25 0.3.0 cannot be upgraded in place to 1.0.0'
        USING ERRCODE = 'feature_not_supported',
              DETAIL = 'The bm25vector API and its indexes are incompatible with the tsvector engine of 1.0.0.',
              HINT = 'Keep the 0.3.0 binary while exporting source text. Install 1.0.0 in a separate PostgreSQL installation with its own library directory or container, create a new database, regenerate tsvector values from that text, and rebuild all BM25 indexes. See the README migration instructions.';
END
$$;
