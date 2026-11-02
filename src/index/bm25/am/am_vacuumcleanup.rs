// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU Affero General Public License v3 (AGPLv3).
// See <https://www.gnu.org/licenses/agpl-3.0.html> for details.
//
// Copyright (c) 2025-2026 TensorChord Inc.
// Copyright (c) 2026 PGSTY PTE. LTD.
// Maintained by Ruohang Feng @ PGSTY (rh@vonng.com)

use crate::index::storage::PostgresRelation;
use crate::index::temp::{tempdir, tempfile};

#[pgrx::pg_guard]
pub unsafe extern "C-unwind" fn amvacuumcleanup(
    info: *mut pgrx::pg_sys::IndexVacuumInfo,
    stats: *mut pgrx::pg_sys::IndexBulkDeleteResult,
) -> *mut pgrx::pg_sys::IndexBulkDeleteResult {
    let mut stats = stats;
    if stats.is_null() {
        stats = unsafe {
            pgrx::pg_sys::palloc0(size_of::<pgrx::pg_sys::IndexBulkDeleteResult>()).cast()
        };
    }
    let index_relation = unsafe { (*info).index };
    let index = unsafe { PostgresRelation::new(index_relation) };
    let check = || unsafe {
        #[cfg(any(feature = "pg14", feature = "pg15", feature = "pg16", feature = "pg17"))]
        pgrx::pg_sys::vacuum_delay_point();
        #[cfg(feature = "pg18")]
        pgrx::pg_sys::vacuum_delay_point(false);
    };
    let tempdir = tempdir();
    let tempfile = tempfile();
    bm25::maintain(&index, check, tempdir.path(), tempfile.path());
    stats
}
