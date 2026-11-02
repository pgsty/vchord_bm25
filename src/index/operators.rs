// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU Affero General Public License v3 (AGPLv3).
// See <https://www.gnu.org/licenses/agpl-3.0.html> for details.
//
// Copyright (c) 2025-2026 TensorChord Inc.
// Copyright (c) 2026 PGSTY PTE. LTD.
// Maintained by Ruohang Feng @ PGSTY (rh@vonng.com)

use crate::datatype::memory_tsvector::{TsVectorInput, TsVectorOutput};
use crate::datatype::tsvector::{cast_tsvector_to_document, cast_tsvector_to_query};
use crate::index::storage::PostgresRelation;
use pgrx::datum::IntoDatum;
use pgrx::pg_sys::Oid;
use std::ffi::{CStr, CString, c_char};
use std::num::NonZero;

struct ClassInfo {
    relname: CString,
    relam: Oid,
    relkind: c_char,
}

unsafe fn heap_tuple_struct<T>(tuple: pgrx::pg_sys::HeapTuple) -> *const T {
    let tuple = unsafe { &*tuple };
    let data = tuple.t_data;
    unsafe { data.cast::<u8>().add((*data).t_hoff as usize).cast::<T>() }
}

fn am_oid_by_name(amname: &CStr) -> Option<Oid> {
    let tuple = unsafe {
        pgrx::pg_sys::SearchSysCache1(
            pgrx::pg_sys::SysCacheIdentifier::AMNAME as i32,
            amname.into_datum()?,
        )
    };
    if tuple.is_null() {
        return None;
    }
    let oid = unsafe {
        let am = heap_tuple_struct::<pgrx::pg_sys::FormData_pg_am>(tuple);
        (*am).oid
    };
    unsafe { pgrx::pg_sys::ReleaseSysCache(tuple) };
    Some(oid)
}

fn class_info(reloid: Oid) -> Option<ClassInfo> {
    let tuple = unsafe {
        pgrx::pg_sys::SearchSysCache1(
            pgrx::pg_sys::SysCacheIdentifier::RELOID as i32,
            reloid.into_datum()?,
        )
    };
    if tuple.is_null() {
        return None;
    }
    let info = unsafe {
        let class = heap_tuple_struct::<pgrx::pg_sys::FormData_pg_class>(tuple);
        ClassInfo {
            relname: CStr::from_ptr((*class).relname.data.as_ptr()).to_owned(),
            relam: (*class).relam,
            relkind: (*class).relkind,
        }
    };
    unsafe { pgrx::pg_sys::ReleaseSysCache(tuple) };
    Some(info)
}

#[pgrx::pg_extern(stable, strict, parallel_safe)]
pub fn _bm25_evaluate(lhs: TsVectorInput, rhs: pgrx::composite_type!("bm25query")) -> f64 {
    let vector: TsVectorOutput = match rhs.get_by_index(NonZero::new(1).unwrap()) {
        Ok(Some(s)) => s,
        Ok(None) => pgrx::error!("bm25query contains a null vector"),
        Err(_) => unreachable!(),
    };
    let index: Oid = match rhs.get_by_index(NonZero::new(2).unwrap()) {
        Ok(Some(s)) => s,
        Ok(None) => pgrx::error!("bm25query contains a null index"),
        Err(_) => unreachable!(),
    };
    let Some(pg_am_oid) = am_oid_by_name(c"bm25") else {
        pgrx::error!("vchord_bm25 is not installed");
    };
    let Some(pg_class) = class_info(index) else {
        pgrx::error!("the relation does not exist");
    };
    if pg_class.relkind != pgrx::pg_sys::RELKIND_INDEX as c_char {
        pgrx::error!("the relation {:?} is not an index", pg_class.relname);
    }
    if pg_class.relam != pg_am_oid {
        pgrx::error!("the index {:?} is not a bm25 index", pg_class.relname);
    }
    let relation = Index::open(index, pgrx::pg_sys::AccessShareLock as _);
    let index = unsafe { PostgresRelation::new(relation.raw()) };
    let seed = bm25::seed::seed(&index);
    let lhs = cast_tsvector_to_document(&seed, lhs.as_borrowed());
    let rhs = cast_tsvector_to_query(&seed, vector.as_borrowed());
    let score = bm25::evaluate(&index, &lhs, &rhs);
    -score.to_f64()
}

struct Index {
    raw: *mut pgrx::pg_sys::RelationData,
    lockmode: pgrx::pg_sys::LOCKMODE,
}

impl Index {
    fn open(indexrelid: Oid, lockmode: pgrx::pg_sys::LOCKMASK) -> Self {
        Self {
            raw: unsafe { pgrx::pg_sys::index_open(indexrelid, lockmode) },
            lockmode,
        }
    }
    fn raw(&self) -> *mut pgrx::pg_sys::RelationData {
        self.raw
    }
}

impl Drop for Index {
    fn drop(&mut self) {
        unsafe {
            pgrx::pg_sys::index_close(self.raw, self.lockmode);
        }
    }
}
