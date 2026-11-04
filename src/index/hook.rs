// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU Affero General Public License v3 (AGPLv3).
// See <https://www.gnu.org/licenses/agpl-3.0.html> for details.
//
// Copyright (c) 2025-2026 TensorChord Inc.
// Copyright (c) 2026 PGSTY PTE. LTD.
// Maintained by Ruohang Feng @ PGSTY (rh@vonng.com)

#[pgrx::pg_guard]
unsafe extern "C-unwind" fn rewrite_plan_state(
    node: *mut pgrx::pg_sys::PlanState,
    context: *mut core::ffi::c_void,
) -> bool {
    unsafe fn dirty_check_bm25(index_relation: *mut pgrx::pg_sys::RelationData) -> Option<bool> {
        type FnPtr = unsafe extern "C-unwind" fn(
            *mut pgrx::pg_sys::RelationData,
            i32,
            i32,
        ) -> *mut pgrx::pg_sys::IndexScanDescData;
        unsafe {
            let index_relation = index_relation.as_ref()?;
            let indam = index_relation.rd_indam.as_ref()?;
            let ambeginscan = indam.ambeginscan.as_ref()?;
            Some(core::ptr::fn_addr_eq::<FnPtr, FnPtr>(
                *ambeginscan,
                crate::index::bm25::am::ambeginscan,
            ))
        }
    }

    unsafe {
        if (*node).type_ == pgrx::pg_sys::NodeTag::T_IndexScanState {
            let node = node as *mut pgrx::pg_sys::IndexScanState;
            let index_relation = (*node).iss_RelationDesc;
            if (*node).iss_ScanDesc.is_null() {
                if Some(true) == dirty_check_bm25(index_relation) {
                    use crate::index::bm25::am::Scanner;

                    (*node).iss_ScanDesc = pgrx::pg_sys::index_beginscan(
                        (*node).ss.ss_currentRelation,
                        (*node).iss_RelationDesc,
                        (*(*node).ss.ps.state).es_snapshot,
                        #[cfg(any(feature = "pg18", feature = "pg19"))]
                        std::ptr::null_mut(),
                        (*node).iss_NumScanKeys,
                        (*node).iss_NumOrderByKeys,
                        #[cfg(feature = "pg19")]
                        0,
                    );

                    let scanner = &mut *((*(*node).iss_ScanDesc).opaque as *mut Scanner);
                    scanner.hack = std::ptr::NonNull::new(node);

                    if (*node).iss_NumRuntimeKeys == 0 || (*node).iss_RuntimeKeysReady {
                        pgrx::pg_sys::index_rescan(
                            (*node).iss_ScanDesc,
                            (*node).iss_ScanKeys,
                            (*node).iss_NumScanKeys,
                            (*node).iss_OrderByKeys,
                            (*node).iss_NumOrderByKeys,
                        );
                    }
                }
            }
        }
        pgrx::pg_sys::planstate_tree_walker(node, Some(rewrite_plan_state), context)
    }
}

static mut PREV_EXECUTOR_RUN: pgrx::pg_sys::ExecutorRun_hook_type = None;
static mut PREV_EXECUTOR_FINISH: pgrx::pg_sys::ExecutorFinish_hook_type = None;

static mut PREV_EXECUTOR_START: pgrx::pg_sys::ExecutorStart_hook_type = None;

#[pgrx::pg_guard]
unsafe extern "C-unwind" fn executor_start(
    query_desc: *mut pgrx::pg_sys::QueryDesc,
    eflags: core::ffi::c_int,
) {
    let lease = crate::index::query::begin(query_desc);
    let _active = crate::index::query::enter(query_desc);
    unsafe {
        use core::ptr::null_mut;
        use pgrx::pg_sys::submodules::ffi::pg_guard_ffi_boundary;
        if let Some(prev_executor_start) = PREV_EXECUTOR_START {
            #[allow(ffi_unwind_calls, reason = "protected by pg_guard_ffi_boundary")]
            pg_guard_ffi_boundary(|| prev_executor_start(query_desc, eflags))
        } else {
            pgrx::pg_sys::standard_ExecutorStart(query_desc, eflags)
        }
        pgrx::memcxt::PgMemoryContexts::For((*(*query_desc).estate).es_query_cxt)
            .leak_and_drop_on_delete(lease);
        pg_guard_ffi_boundary(|| rewrite_plan_state((*query_desc).planstate, null_mut()));
    }
}

#[cfg(any(feature = "pg14", feature = "pg15", feature = "pg16", feature = "pg17"))]
#[pgrx::pg_guard]
unsafe extern "C-unwind" fn executor_run(
    desc: *mut pgrx::pg_sys::QueryDesc,
    direction: pgrx::pg_sys::ScanDirection::Type,
    count: u64,
    execute_once: bool,
) {
    let _active = crate::index::query::enter(desc);
    unsafe {
        use pgrx::pg_sys::submodules::ffi::pg_guard_ffi_boundary;
        if let Some(run) = PREV_EXECUTOR_RUN {
            #[allow(ffi_unwind_calls, reason = "protected by pg_guard_ffi_boundary")]
            pg_guard_ffi_boundary(|| run(desc, direction, count, execute_once));
        } else {
            pgrx::pg_sys::standard_ExecutorRun(desc, direction, count, execute_once);
        }
    }
}

#[cfg(any(feature = "pg18", feature = "pg19"))]
#[pgrx::pg_guard]
unsafe extern "C-unwind" fn executor_run(
    desc: *mut pgrx::pg_sys::QueryDesc,
    direction: pgrx::pg_sys::ScanDirection::Type,
    count: u64,
) {
    let _active = crate::index::query::enter(desc);
    unsafe {
        use pgrx::pg_sys::submodules::ffi::pg_guard_ffi_boundary;
        if let Some(run) = PREV_EXECUTOR_RUN {
            #[allow(ffi_unwind_calls, reason = "protected by pg_guard_ffi_boundary")]
            pg_guard_ffi_boundary(|| run(desc, direction, count));
        } else {
            pgrx::pg_sys::standard_ExecutorRun(desc, direction, count);
        }
    }
}

#[pgrx::pg_guard]
unsafe extern "C-unwind" fn executor_finish(desc: *mut pgrx::pg_sys::QueryDesc) {
    let _active = crate::index::query::enter(desc);
    unsafe {
        use pgrx::pg_sys::submodules::ffi::pg_guard_ffi_boundary;
        if let Some(finish) = PREV_EXECUTOR_FINISH {
            #[allow(ffi_unwind_calls, reason = "protected by pg_guard_ffi_boundary")]
            pg_guard_ffi_boundary(|| finish(desc));
        } else {
            pgrx::pg_sys::standard_ExecutorFinish(desc);
        }
    }
}

pub fn init() {
    assert!(crate::is_main());
    unsafe {
        PREV_EXECUTOR_RUN = pgrx::pg_sys::ExecutorRun_hook;
        pgrx::pg_sys::ExecutorRun_hook = Some(executor_run);
        PREV_EXECUTOR_FINISH = pgrx::pg_sys::ExecutorFinish_hook;
        pgrx::pg_sys::ExecutorFinish_hook = Some(executor_finish);
        PREV_EXECUTOR_START = pgrx::pg_sys::ExecutorStart_hook;
        pgrx::pg_sys::ExecutorStart_hook = Some(executor_start);
    }
}
