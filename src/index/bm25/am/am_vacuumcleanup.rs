// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU Affero General Public License v3 (AGPLv3).
// See <https://www.gnu.org/licenses/agpl-3.0.html> for details.
//
// Copyright (c) 2025-2026 TensorChord Inc.
// Copyright (c) 2026 PGSTY PTE. LTD.
// Maintained by Ruohang Feng @ PGSTY (rh@vonng.com)

use super::Reloption;
use crate::index::storage::PostgresRelation;
use crate::index::temp::{tempdir, tempfile};
use index::relation::RelationWrite;
use pgrx::pg_sys;

#[pgrx::pg_guard]
pub unsafe extern "C-unwind" fn amvacuumcleanup(
    info: *mut pg_sys::IndexVacuumInfo,
    stats: *mut pg_sys::IndexBulkDeleteResult,
) -> *mut pg_sys::IndexBulkDeleteResult {
    // ANALYZE must neither change index contents nor replace valid estimates.
    if unsafe { (*info).analyze_only } {
        return stats;
    }
    let stats = if stats.is_null() {
        unsafe { pg_sys::palloc0(size_of::<pg_sys::IndexBulkDeleteResult>()).cast() }
    } else {
        stats
    };
    #[cfg(any(feature = "pg14", feature = "pg15"))]
    let heaprel = std::ptr::null_mut();
    #[cfg(any(feature = "pg16", feature = "pg17", feature = "pg18", feature = "pg19"))]
    let heaprel = unsafe { (*info).heaprel };
    let check = || unsafe {
        #[cfg(any(feature = "pg14", feature = "pg15", feature = "pg16", feature = "pg17"))]
        pg_sys::vacuum_delay_point();
        #[cfg(any(feature = "pg18", feature = "pg19"))]
        pg_sys::vacuum_delay_point(false);
    };
    let raw = unsafe { (*info).index };
    let maintained = run(raw, heaprel, false, check);
    let index = unsafe { PostgresRelation::<bm25::Opaque>::new(raw) };
    unsafe {
        (*stats).num_pages = index.npages();
        (*stats).num_index_tuples = maintained.live_rows as f64;
        // Concurrent inserts can append after the captured boundary.
        (*stats).estimated_count = true;
    }
    stats
}

fn run(
    raw: pg_sys::Relation,
    heaprel: pg_sys::Relation,
    force: bool,
    check: impl Fn(),
) -> bm25::Maintained {
    // The caller holds the heap's ShareUpdateExclusiveLock. This serializes
    // maintainers without excluding readers, insertions or parallel VACUUM.
    let index = unsafe { PostgresRelation::new(raw) };
    let policy = unsafe { Reloption::maintenance((*raw).rd_options.cast(), force) };
    let can_recycle = |safexid| unsafe {
        pg_sys::GlobalVisCheckRemovableFullXid(
            heaprel,
            pg_sys::FullTransactionId { value: safexid },
        )
    };
    let next_xid = || unsafe { pg_sys::ReadNextFullTransactionId().value };
    let dir = tempdir();
    let file = tempfile();
    let result = bm25::maintain(
        &index,
        check,
        dir.path(),
        file.path(),
        can_recycle,
        next_xid,
        policy,
    );
    pgrx::debug1!(
        "bm25 maintenance: rebuilt={} recycled={} orphans={} live_rows={}",
        result.rebuilt,
        result.recycled,
        result.orphans,
        result.live_rows
    );
    result
}

#[pgrx::pg_extern(sql = "")]
fn _bm25_maintain(index: pg_sys::Oid) -> bool {
    unsafe {
        let heap = pg_sys::IndexGetRelation(index, false);
        let role = pg_sys::GetUserId();
        #[cfg(any(feature = "pg14", feature = "pg15"))]
        let allowed = pg_sys::pg_class_ownercheck(heap, role);
        #[cfg(any(feature = "pg16", feature = "pg17", feature = "pg18", feature = "pg19"))]
        let allowed = pg_sys::object_ownercheck(pg_sys::RelationRelationId, heap, role);
        #[cfg(any(feature = "pg17", feature = "pg18", feature = "pg19"))]
        let allowed = allowed
            || pg_sys::pg_class_aclcheck(heap, role, pg_sys::ACL_MAINTAIN as _)
                == pg_sys::AclResult::ACLCHECK_OK;
        if !allowed {
            pg_sys::aclcheck_error(
                pg_sys::AclResult::ACLCHECK_NOT_OWNER,
                pg_sys::ObjectType::OBJECT_TABLE,
                pg_sys::get_rel_name(heap),
            );
        }
        let heap = pgrx::PgRelation::with_lock(heap, pg_sys::ShareUpdateExclusiveLock as _);
        let relation = pgrx::PgRelation::with_lock(index, pg_sys::AccessShareLock as _);
        if (*(*relation.as_ptr()).rd_rel).relam != pg_sys::get_am_oid(c"bm25".as_ptr(), false) {
            pgrx::error!("the index is not a bm25 index");
        }
        run(relation.as_ptr(), heap.as_ptr(), true, || {
            pg_sys::check_for_interrupts!();
        })
        .rebuilt
    }
}
