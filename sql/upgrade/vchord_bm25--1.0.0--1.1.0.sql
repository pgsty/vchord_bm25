-- vchord_bm25--1.0.0--1.1.0.sql

-- complain if this file is sourced in psql, rather than via ALTER EXTENSION
\echo Use "ALTER EXTENSION vchord_bm25 UPDATE TO '1.1.0'" to load this file. \quit

-- 1.1.0 keeps the 1.0.0 SQL API and format 4 index storage. The upgrade is
-- intentionally empty; replacing the library fixes WAND pruning and does not
-- require REINDEX for a healthy 1.0.0 index.
