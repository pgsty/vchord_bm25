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
