// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU Affero General Public License v3 (AGPLv3).
// See <https://www.gnu.org/licenses/agpl-3.0.html> for details.
//
// Copyright (c) 2025-2026 TensorChord Inc.
// Copyright (c) 2026 PGSTY PTE. LTD.
// Maintained by Ruohang Feng @ PGSTY (rh@vonng.com)

mod bm25;
mod fetcher;
mod gucs;
mod hook;
mod operators;
mod query;
mod storage;
mod temp;
mod traverse;

pub fn init() {
    gucs::init();
    hook::init();
    bm25::am::init();
}

/// Cost given to index paths the access method cannot execute (no matching
/// BM25 ORDER BY key, multiple ORDER BY keys, or index-only scans). Finite on
/// purpose: `inf - inf` in PostgreSQL's `cost_index()` yields NaN.
pub const UNUSABLE_PATH_COST: f64 = 1.0e30;

/// Mirror PostgreSQL's `pgstat_count_index_scan(rel)` macro so
/// `pg_stat_user_indexes.idx_scan` tracks index searches across PG 15–19.
#[inline]
pub(crate) unsafe fn count_index_scan(rel: pgrx::pg_sys::Relation) {
    unsafe {
        if (*rel).pgstat_info.is_null() {
            if !(*rel).pgstat_enabled {
                return;
            }
            pgrx::pg_sys::pgstat_assoc_relation(rel);
        }
        #[cfg(feature = "pg15")]
        {
            (*(*rel).pgstat_info).t_counts.t_numscans += 1;
        }
        #[cfg(any(feature = "pg16", feature = "pg17", feature = "pg18", feature = "pg19"))]
        {
            (*(*rel).pgstat_info).counts.numscans += 1;
        }
    }
}

/// Whether a destructor may release PostgreSQL-managed resources directly.
///
/// During panic unwinding (after PostgreSQL `ERROR`) or backend exit,
/// PostgreSQL's own transaction abort / exit path owns resource cleanup.
#[inline]
pub(crate) fn pg_cleanup_allowed() -> bool {
    !std::thread::panicking()
        && unsafe { !pgrx::pg_sys::proc_exit_inprogress && pgrx::pg_sys::IsTransactionState() }
}

/// A handler's result: the routine itself where PostgreSQL takes it as static, else a copy it frees.
#[inline(always)]
pub unsafe fn am_routine(routine: &'static pgrx::pg_sys::IndexAmRoutine) -> pgrx::pg_sys::Datum {
    #[cfg(feature = "pg19")]
    let routine = std::ptr::from_ref(routine).cast_mut();
    #[cfg(not(feature = "pg19"))]
    let routine = unsafe {
        unsafe extern "C" {
            fn palloc(size: usize) -> *mut std::ffi::c_void;
        }
        let copy = palloc(size_of::<pgrx::pg_sys::IndexAmRoutine>())
            .cast::<pgrx::pg_sys::IndexAmRoutine>();
        copy.copy_from_nonoverlapping(routine, 1);
        copy
    };
    pgrx::pg_sys::Datum::from(routine)
}

/// Switches `CurrentMemoryContext` and restores it when dropped, including
/// during unwinding.
pub struct MemoryContextSwitch(pgrx::pg_sys::MemoryContext);

impl MemoryContextSwitch {
    pub unsafe fn new(context: pgrx::pg_sys::MemoryContext) -> Self {
        Self(unsafe { pgrx::pg_sys::MemoryContextSwitchTo(context) })
    }
}

impl Drop for MemoryContextSwitch {
    fn drop(&mut self) {
        unsafe {
            pgrx::pg_sys::MemoryContextSwitchTo(self.0);
        }
    }
}
