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

// Both scalar scoring and native scans read statistics for the entire index.
// Query preparation checks the current role once per execution/query key;
// a later execution or a different role must perform its own permission check.
pub(crate) fn check_index_permissions(index: Oid) {
    let (table, user) = unsafe {
        let table = pg_sys::IndexGetRelation(index, false);
        // Scalar calls need the same protection against table/RLS DDL as scans.
        // Keep this lock until transaction end, including while scoring.
        pg_sys::LockRelationOid(table, pg_sys::AccessShareLock as _);
        (table, pg_sys::GetUserId())
    };
    if unsafe { pg_sys::pg_class_aclcheck(table, user, pg_sys::ACL_SELECT as _) }
        != pg_sys::AclResult::ACLCHECK_OK
    {
        let attribute = unsafe {
            let tuple = pg_sys::SearchSysCache1(
                pg_sys::SysCacheIdentifier::INDEXRELID as _,
                index.into_datum().unwrap(),
            );
            if tuple.is_null() {
                pgrx::error!("the index {index} does not exist");
            }
            let info = heap_tuple_struct::<pg_sys::FormData_pg_index>(tuple);
            let mut no_predicate = false;
            pg_sys::SysCacheGetAttr(
                pg_sys::SysCacheIdentifier::INDEXRELID as _,
                tuple,
                pg_sys::Anum_pg_index_indpred as _,
                &mut no_predicate,
            );
            // Expressions (attnum = 0) and partial predicates require table SELECT.
            let attribute = if (*info).indnatts == 1 && no_predicate {
                *(*info).indkey.values.as_ptr()
            } else {
                0
            };
            pg_sys::ReleaseSysCache(tuple);
            attribute
        };
        if attribute <= 0
            || unsafe {
                pg_sys::pg_attribute_aclcheck(table, attribute, user, pg_sys::ACL_SELECT as _)
            } != pg_sys::AclResult::ACLCHECK_OK
        {
            unsafe {
                pg_sys::aclcheck_error(
                    pg_sys::AclResult::ACLCHECK_NO_PRIV,
                    pg_sys::ObjectType::OBJECT_TABLE,
                    pg_sys::get_rel_name(table),
                );
            }
        }
    }
    // noError=true also rejects row_security=off; RLS_NONE_ENV permits actual
    // owner/BYPASSRLS exemptions, but RLS_ENABLED cannot expose global DF.
    // C enum constants are i32 on MSVC and u32 elsewhere.
    let rls_enabled: i32 = pg_sys::CheckEnableRlsResult::RLS_ENABLED as _;
    if unsafe { pg_sys::check_enable_rls(table, pg_sys::InvalidOid, true) } == rls_enabled {
        pgrx::ereport!(
            ERROR,
            pgrx::PgSqlErrorCode::ERRCODE_INSUFFICIENT_PRIVILEGE,
            "bm25 cannot expose global corpus statistics under row-level security"
        );
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
    let index = unsafe { PostgresRelation::new(relation.raw()) };
    let seed = bm25::seed::seed(&index);
    let lhs = cast_tsvector_to_document(&seed, lhs.as_borrowed());
    let rhs = cast_tsvector_to_query(&seed, vector.as_borrowed());
    let prepared = crate::index::query::prepare(&index, relation.oid(), &rhs);
    // Subtract instead of negating, so a zero score is +0 rather than -0.
    0.0 - prepared.evaluate(&lhs)
}

struct Index {
    raw: *mut pg_sys::RelationData,
}

impl Index {
    fn open(indexrelid: Oid, lockmode: pg_sys::LOCKMASK) -> Self {
        Self {
            raw: unsafe { pg_sys::index_open(indexrelid, lockmode) },
        }
    }
    fn oid(&self) -> Oid {
        unsafe { (*self.raw).rd_id }
    }
    fn raw(&self) -> *mut pg_sys::RelationData {
        self.raw
    }
}

impl Drop for Index {
    fn drop(&mut self) {
        unsafe {
            // Cached segment addresses remain tied to this physical index until
            // the statement/transaction finishes, including concurrent REINDEX.
            pg_sys::index_close(self.raw, pg_sys::NoLock as _);
        }
    }
}
