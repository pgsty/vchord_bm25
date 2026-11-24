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
use pgrx::pg_sys::{self, Oid};
use std::ffi::{CStr, CString, c_char};
use std::num::NonZero;

struct ClassInfo {
    relname: CString,
    relam: Oid,
    relkind: c_char,
}

unsafe fn heap_tuple_struct<T>(tuple: pg_sys::HeapTuple) -> *const T {
    let tuple = unsafe { &*tuple };
    let data = tuple.t_data;
    unsafe { data.cast::<u8>().add((*data).t_hoff as usize).cast::<T>() }
}

fn am_oid_by_name(amname: &CStr) -> Option<Oid> {
    let tuple = unsafe {
        pg_sys::SearchSysCache1(
            pg_sys::SysCacheIdentifier::AMNAME as _,
            amname.into_datum()?,
        )
    };
    if tuple.is_null() {
        return None;
    }
    let oid = unsafe {
        let am = heap_tuple_struct::<pg_sys::FormData_pg_am>(tuple);
        (*am).oid
    };
    unsafe { pg_sys::ReleaseSysCache(tuple) };
    Some(oid)
}

fn class_info(reloid: Oid) -> Option<ClassInfo> {
    let tuple = unsafe {
        pg_sys::SearchSysCache1(
            pg_sys::SysCacheIdentifier::RELOID as _,
            reloid.into_datum()?,
        )
    };
    if tuple.is_null() {
        return None;
    }
    let info = unsafe {
        let class = heap_tuple_struct::<pg_sys::FormData_pg_class>(tuple);
        ClassInfo {
            relname: CStr::from_ptr((*class).relname.data.as_ptr()).to_owned(),
            relam: (*class).relam,
            relkind: (*class).relkind,
        }
    };
    unsafe { pg_sys::ReleaseSysCache(tuple) };
    Some(info)
}

fn type_name(typid: Oid) -> Option<CString> {
    let tuple = unsafe {
        pg_sys::SearchSysCache1(
            pg_sys::SysCacheIdentifier::TYPEOID as _,
            typid.into_datum()?,
        )
    };
    if tuple.is_null() {
        return None;
    }
    let name = unsafe {
        let typ = heap_tuple_struct::<pg_sys::FormData_pg_type>(tuple);
        CStr::from_ptr((*typ).typname.data.as_ptr()).to_owned()
    };
    unsafe { pg_sys::ReleaseSysCache(tuple) };
    Some(name)
}

/// Whether `funcid` is this extension's `to_bm25query(tsvector, regclass)`
/// returning its `bm25query`, in whatever schema the extension lives. A
/// function of the same name elsewhere may return a different index.
pub(crate) fn is_bm25query_constructor(funcid: Oid) -> bool {
    unsafe {
        let name = pg_sys::get_func_name(funcid);
        if name.is_null() || CStr::from_ptr(name) != c"to_bm25query" {
            return false;
        }
        let mut argtypes = std::ptr::null_mut::<Oid>();
        let mut nargs = 0;
        let rettype = pg_sys::get_func_signature(funcid, &mut argtypes, &mut nargs);
        if nargs != 2
            || *argtypes.add(0) != pg_sys::TSVECTOROID
            || *argtypes.add(1) != pg_sys::REGCLASSOID
        {
            return false;
        }
        let extension = pg_sys::get_extension_oid(c"vchord_bm25".as_ptr(), true);
        extension != pg_sys::InvalidOid
            && pg_sys::getExtensionOfObject(pg_sys::ProcedureRelationId, funcid) == extension
            && pg_sys::getExtensionOfObject(pg_sys::TypeRelationId, rettype) == extension
            && type_name(rettype).as_deref() == Some(c"bm25query")
    }
}

#[pgrx::pg_extern(stable, strict, parallel_restricted, cost = 100)]
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
    let seed = crate::index::query::seed(index);
    let lhs = cast_tsvector_to_document(&seed, lhs.as_borrowed());
    let rhs = cast_tsvector_to_query(&seed, vector.as_borrowed());
    let prepared = crate::index::query::prepare(index, &rhs, None);
    // Subtract instead of negating, so a zero score is +0 rather than -0.
    0.0 - prepared.evaluate(&lhs)
}

pub(super) fn load_seed(index: Oid) -> [u8; 32] {
    let Some(pg_am_oid) = am_oid_by_name(c"bm25") else {
        pgrx::error!("vchord_bm25 is not installed");
    };
    let Some(pg_class) = class_info(index) else {
        pgrx::error!("the relation does not exist");
    };
    if pg_class.relkind != pg_sys::RELKIND_INDEX as c_char {
        pgrx::error!("the relation {:?} is not an index", pg_class.relname);
    }
    if pg_class.relam != pg_am_oid {
        pgrx::error!("the index {:?} is not a bm25 index", pg_class.relname);
    }
    // Lock the table before its index, in the order DROP INDEX and the
    // executor use, so concurrent DDL cannot deadlock with scoring.
    unsafe {
        pg_sys::LockRelationOid(
            pg_sys::IndexGetRelation(index, false),
            pg_sys::AccessShareLock as _,
        );
    }
    let relation = Index::open(index, pg_sys::AccessShareLock as _);
    // Check permissions and validity before reading any index page. An index
    // left by a failed CREATE INDEX CONCURRENTLY may have no metapage, or
    // incomplete corpus statistics.
    unsafe { super::permissions::check(relation.raw()) };
    if !relation.is_valid() {
        pg_sys::panic::ErrorReport::new(
            pgrx::PgSqlErrorCode::ERRCODE_OBJECT_NOT_IN_PREREQUISITE_STATE,
            format!(
                "index \"{}\" is not valid",
                pg_class.relname.to_string_lossy()
            ),
            "_bm25_evaluate",
        )
        .set_hint("REINDEX the index or drop it.")
        .report(pg_sys::elog::PgLogLevel::ERROR);
        unreachable!();
    }
    let index = unsafe { PostgresRelation::<bm25::Opaque>::new(relation.raw()) };
    bm25::seed::seed(&index)
}

pub(super) struct Index {
    raw: *mut pg_sys::RelationData,
}

impl Index {
    pub(super) fn open(indexrelid: Oid, lockmode: pg_sys::LOCKMASK) -> Self {
        Self {
            raw: unsafe { pg_sys::index_open(indexrelid, lockmode) },
        }
    }
    pub(super) fn raw(&self) -> *mut pg_sys::RelationData {
        self.raw
    }
    fn is_valid(&self) -> bool {
        unsafe { (*(*self.raw).rd_index).indisvalid }
    }
}

impl Drop for Index {
    fn drop(&mut self) {
        if crate::index::pg_cleanup_allowed() {
            unsafe {
                // Cached segment addresses remain tied to this physical index until
                // the statement/transaction finishes, including concurrent REINDEX.
                pg_sys::index_close(self.raw, pg_sys::NoLock as _);
            }
        }
    }
}
