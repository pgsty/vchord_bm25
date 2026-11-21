// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU Affero General Public License v3 (AGPLv3).
// See <https://www.gnu.org/licenses/agpl-3.0.html> for details.
//
// Copyright (c) 2025-2026 TensorChord Inc.
// Copyright (c) 2026 PGSTY PTE. LTD.
// Maintained by Ruohang Feng @ PGSTY (rh@vonng.com)

use pgrx::pg_sys::{BlockIdData, ItemPointerData};
use std::cell::LazyCell;
use std::ops::DerefMut;
use std::ptr::NonNull;

pub trait FilterableTuple {
    fn filter(&mut self) -> bool;
}

pub trait Fetcher {
    type Tuple<'a>: FilterableTuple
    where
        Self: 'a;

    fn fetch(&mut self, key: [u16; 3]) -> Option<Self::Tuple<'_>>;
}

impl<T: Fetcher, F: FnOnce() -> T> Fetcher for LazyCell<T, F> {
    type Tuple<'a>
        = T::Tuple<'a>
    where
        Self: 'a;

    fn fetch(&mut self, key: [u16; 3]) -> Option<Self::Tuple<'_>> {
        self.deref_mut().fetch(key)
    }
}

pub struct HeapFetcher {
    heap_relation: pgrx::pg_sys::Relation,
    snapshot: pgrx::pg_sys::Snapshot,
    heapfetch: *mut pgrx::pg_sys::IndexFetchTableData,
    slot: *mut pgrx::pg_sys::TupleTableSlot,
    hack: *mut pgrx::pg_sys::IndexScanState,
}

impl HeapFetcher {
    pub unsafe fn new(
        heap_relation: pgrx::pg_sys::Relation,
        snapshot: pgrx::pg_sys::Snapshot,
        heapfetch: *mut pgrx::pg_sys::IndexFetchTableData,
        hack: *mut pgrx::pg_sys::IndexScanState,
    ) -> Self {
        unsafe {
            Self {
                heap_relation,
                snapshot,
                heapfetch,
                slot: pgrx::pg_sys::table_slot_create(heap_relation, std::ptr::null_mut()),
                hack,
            }
        }
    }
}

impl Drop for HeapFetcher {
    fn drop(&mut self) {
        if crate::index::pg_cleanup_allowed() {
            unsafe {
                pgrx::pg_sys::ExecDropSingleTupleTableSlot(self.slot);
            }
        }
    }
}

impl Fetcher for HeapFetcher {
    type Tuple<'a> = HeapTuple<'a>;

    fn fetch(&mut self, key: [u16; 3]) -> Option<Self::Tuple<'_>> {
        unsafe {
            use pgrx::pg_sys::ffi::pg_guard_ffi_boundary;
            let mut ctid = key_to_ctid(key);
            let table_am = (*self.heap_relation).rd_tableam;
            if table_am.is_null() {
                panic!("unknown heap access method");
            }
            let index_fetch_tuple = (*table_am)
                .index_fetch_tuple
                .expect("unsupported heap access method");
            let found = 'a: {
                let mut call_again = false;
                let mut all_dead = false;
                #[allow(ffi_unwind_calls, reason = "protected by pg_guard_ffi_boundary")]
                let found = pg_guard_ffi_boundary(|| {
                    index_fetch_tuple(
                        self.heapfetch,
                        &mut ctid,
                        self.snapshot,
                        self.slot,
                        &mut call_again,
                        &mut all_dead,
                    )
                });
                if found {
                    break 'a true;
                }
                while call_again {
                    #[allow(ffi_unwind_calls, reason = "protected by pg_guard_ffi_boundary")]
                    let found = pg_guard_ffi_boundary(|| {
                        index_fetch_tuple(
                            self.heapfetch,
                            &mut ctid,
                            self.snapshot,
                            self.slot,
                            &mut call_again,
                            &mut all_dead,
                        )
                    });
                    if found {
                        break 'a true;
                    }
                }
                false
            };
            if found {
                Some(HeapTuple { this: self })
            } else {
                None
            }
        }
    }
}

pub struct HeapTuple<'a> {
    this: &'a mut HeapFetcher,
}

impl Drop for HeapTuple<'_> {
    fn drop(&mut self) {
        if crate::index::pg_cleanup_allowed() {
            unsafe {
                pgrx::pg_sys::ExecClearTuple(self.this.slot);
            }
        }
    }
}

impl FilterableTuple for HeapTuple<'_> {
    fn filter(&mut self) -> bool {
        unsafe {
            use pgrx::datum::FromDatum;
            use pgrx::pg_sys::ffi::pg_guard_ffi_boundary;
            let this = &mut self.this;
            if !this.hack.is_null() {
                if let Some(qual) = NonNull::new((*this.hack).ss.ps.qual) {
                    assert!(qual.as_ref().flags & pgrx::pg_sys::EEO_FLAG_IS_QUAL as u8 != 0);
                    let evalfunc = qual.as_ref().evalfunc.expect("no evalfunc for qual");
                    if !(*this.hack).ss.ps.ps_ExprContext.is_null() {
                        let econtext = (*this.hack).ss.ps.ps_ExprContext;
                        (*econtext).ecxt_scantuple = this.slot;
                        pgrx::pg_sys::MemoryContextReset((*econtext).ecxt_per_tuple_memory);
                        let result = {
                            let _switch = crate::index::MemoryContextSwitch::new(
                                (*econtext).ecxt_per_tuple_memory,
                            );
                            let mut is_null = true;
                            #[allow(
                                ffi_unwind_calls,
                                reason = "protected by pg_guard_ffi_boundary"
                            )]
                            let datum = pg_guard_ffi_boundary(|| {
                                evalfunc(qual.as_ptr(), econtext, &mut is_null)
                            });
                            bool::from_datum(datum, is_null)
                        };
                        if result != Some(true) {
                            return false;
                        }
                    }
                }
            }
            true
        }
    }
}

pub const fn ctid_to_key(
    ItemPointerData {
        ip_blkid: BlockIdData { bi_hi, bi_lo },
        ip_posid,
    }: ItemPointerData,
) -> [u16; 3] {
    [bi_hi, bi_lo, ip_posid]
}

pub const fn key_to_ctid([bi_hi, bi_lo, ip_posid]: [u16; 3]) -> ItemPointerData {
    ItemPointerData {
        ip_blkid: BlockIdData { bi_hi, bi_lo },
        ip_posid,
    }
}
