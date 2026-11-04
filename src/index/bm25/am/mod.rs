// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU Affero General Public License v3 (AGPLv3).
// See <https://www.gnu.org/licenses/agpl-3.0.html> for details.
//
// Copyright (c) 2025-2026 TensorChord Inc.
// Copyright (c) 2026 PGSTY PTE. LTD.
// Maintained by Ruohang Feng @ PGSTY (rh@vonng.com)

mod am_build;
mod am_vacuumcleanup;

use crate::datatype::memory_tsvector::TsVectorInput;
use crate::datatype::tsvector::cast_tsvector_to_document;
use crate::index::bm25::scanners::{DefaultBuilder, SearchOptions};
use crate::index::fetcher::*;
use crate::index::gucs;
use crate::index::storage::PostgresRelation;
use pgrx::datum::Internal;
use pgrx::pg_sys::Datum;
use std::cell::LazyCell;
use std::ffi::CStr;
use std::ptr::NonNull;
use std::sync::OnceLock;

#[repr(C)]
pub struct Reloption {
    vl_len_: i32,
    options: i32,
    limit: i32,
    prefilter: bool,
    maintenance_min_rows: i32,
    maintenance_scale_factor: f64,
}

impl Reloption {
    pub unsafe fn maintenance(this: *const Self, force: bool) -> bm25::Policy {
        bm25::Policy {
            min_rows: if this.is_null() {
                1000
            } else {
                unsafe { (*this).maintenance_min_rows as u32 }
            },
            scale_factor: if this.is_null() {
                0.1
            } else {
                unsafe { (*this).maintenance_scale_factor }
            },
            force,
        }
    }
    unsafe fn options<'a>(this: *const Self, default: &'static CStr) -> &'a CStr {
        unsafe {
            if this.is_null() {
                return default;
            }
            let count = (&raw const (*this).options).read();
            if count == 0 {
                return default;
            }
            let ptr = this.cast::<u8>().add(count as _);
            CStr::from_ptr(ptr.cast())
        }
    }
    pub unsafe fn limit(this: *const Self, default: i32) -> i32 {
        unsafe {
            if this.is_null() {
                return default;
            }
            (*this).limit
        }
    }
    pub unsafe fn prefilter(this: *const Self, default: bool) -> bool {
        unsafe {
            if this.is_null() {
                return default;
            }
            (*this).prefilter
        }
    }
}

const TABLE: &[pgrx::pg_sys::relopt_parse_elt] = &[
    pgrx::pg_sys::relopt_parse_elt {
        optname: c"options".as_ptr(),
        opttype: pgrx::pg_sys::relopt_type::RELOPT_TYPE_STRING,
        offset: std::mem::offset_of!(Reloption, options) as i32,
        #[cfg(feature = "pg18")]
        isset_offset: 0,
    },
    pgrx::pg_sys::relopt_parse_elt {
        optname: c"limit".as_ptr(),
        opttype: pgrx::pg_sys::relopt_type::RELOPT_TYPE_INT,
        offset: std::mem::offset_of!(Reloption, limit) as i32,
        #[cfg(feature = "pg18")]
        isset_offset: 0,
    },
    pgrx::pg_sys::relopt_parse_elt {
        optname: c"prefilter".as_ptr(),
        opttype: pgrx::pg_sys::relopt_type::RELOPT_TYPE_BOOL,
        offset: std::mem::offset_of!(Reloption, prefilter) as i32,
        #[cfg(feature = "pg18")]
        isset_offset: 0,
    },
    pgrx::pg_sys::relopt_parse_elt {
        optname: c"maintenance_min_rows".as_ptr(),
        opttype: pgrx::pg_sys::relopt_type::RELOPT_TYPE_INT,
        offset: std::mem::offset_of!(Reloption, maintenance_min_rows) as i32,
        #[cfg(feature = "pg18")]
        isset_offset: 0,
    },
    pgrx::pg_sys::relopt_parse_elt {
        optname: c"maintenance_scale_factor".as_ptr(),
        opttype: pgrx::pg_sys::relopt_type::RELOPT_TYPE_REAL,
        offset: std::mem::offset_of!(Reloption, maintenance_scale_factor) as i32,
        #[cfg(feature = "pg18")]
        isset_offset: 0,
    },
];

static RELOPT_KIND: OnceLock<pgrx::pg_sys::relopt_kind::Type> = OnceLock::new();

pub fn init() {
    RELOPT_KIND.get_or_init(|| {
        let kind;
        unsafe {
            kind = pgrx::pg_sys::add_reloption_kind();
            pgrx::pg_sys::add_string_reloption(
                kind as _,
                c"options".as_ptr(),
                c"Bm25 index options, represented as a TOML string.".as_ptr(),
                c"".as_ptr(),
                None,
                pgrx::pg_sys::AccessExclusiveLock as pgrx::pg_sys::LOCKMODE,
            );
            pgrx::pg_sys::add_int_reloption(
                kind as _,
                c"limit".as_ptr(),
                c"Initial BM25 scan batch size (zero selects 100)".as_ptr(),
                100,
                0,
                65535,
                pgrx::pg_sys::AccessExclusiveLock as pgrx::pg_sys::LOCKMODE,
            );
            pgrx::pg_sys::add_int_reloption(
                kind as _,
                c"maintenance_min_rows".as_ptr(),
                c"Minimum growing records before VACUUM merges segments".as_ptr(),
                1000,
                0,
                i32::MAX,
                pgrx::pg_sys::ShareUpdateExclusiveLock as _,
            );
            pgrx::pg_sys::add_real_reloption(
                kind as _,
                c"maintenance_scale_factor".as_ptr(),
                c"Growing/sealed or deleted/total fraction that triggers compaction".as_ptr(),
                0.1,
                0.0,
                1.0,
                pgrx::pg_sys::ShareUpdateExclusiveLock as _,
            );
            pgrx::pg_sys::add_bool_reloption(
                kind as _,
                c"prefilter".as_ptr(),
                c"Search parameter `bm25.prefilter`".as_ptr(),
                false,
                pgrx::pg_sys::AccessExclusiveLock as pgrx::pg_sys::LOCKMODE,
            );
        }
        kind
    });
}

#[pgrx::pg_extern(sql = "")]
fn _bm25_amhandler(_fcinfo: pgrx::pg_sys::FunctionCallInfo) -> Internal {
    type T = pgrx::pg_sys::IndexAmRoutine;
    unsafe {
        let index_am_routine = pgrx::pg_sys::palloc0(size_of::<T>()) as *mut T;
        index_am_routine.write(AM_HANDLER);
        Internal::from(Some(Datum::from(index_am_routine)))
    }
}

const AM_HANDLER: pgrx::pg_sys::IndexAmRoutine = const {
    let mut am_routine = unsafe { std::mem::zeroed::<pgrx::pg_sys::IndexAmRoutine>() };

    am_routine.type_ = pgrx::pg_sys::NodeTag::T_IndexAmRoutine;

    am_routine.amsupport = 0;
    am_routine.amcanorderbyop = true;

    #[cfg(any(feature = "pg17", feature = "pg18", feature = "pg19"))]
    {
        am_routine.amcanbuildparallel = false;
    }

    // Index access methods that set `amoptionalkey` to `false`
    // must index all tuples, even if the first column is `NULL`.
    // However, PostgreSQL does not generate a path if there is no
    // index clauses, even if there is a `ORDER BY` clause.
    // So we have to set it to `true` and set costs of every path
    // for vector index scans without `ORDER BY` clauses a large number
    // and throw errors if someone really wants such a path.
    am_routine.amoptionalkey = true;

    am_routine.amvalidate = Some(amvalidate);
    am_routine.amoptions = Some(amoptions);
    am_routine.amcostestimate = Some(amcostestimate);

    am_routine.ambuildphasename = Some(am_build::ambuildphasename);
    am_routine.ambuild = Some(am_build::ambuild);
    am_routine.ambuildempty = Some(am_build::ambuildempty);
    am_routine.aminsert = Some(aminsert);
    am_routine.ambulkdelete = Some(ambulkdelete);
    am_routine.amparallelvacuumoptions = (pgrx::pg_sys::VACUUM_OPTION_PARALLEL_BULKDEL
        | pgrx::pg_sys::VACUUM_OPTION_PARALLEL_CLEANUP)
        as _;
    am_routine.amvacuumcleanup = Some(am_vacuumcleanup::amvacuumcleanup);

    am_routine.ambeginscan = Some(ambeginscan);
    am_routine.amrescan = Some(amrescan);
    am_routine.amgettuple = Some(amgettuple);
    am_routine.amendscan = Some(amendscan);

    am_routine
};

#[pgrx::pg_guard]
pub unsafe extern "C-unwind" fn amvalidate(_opclass_oid: pgrx::pg_sys::Oid) -> bool {
    true
}

#[pgrx::pg_guard]
pub unsafe extern "C-unwind" fn amoptions(
    reloptions: Datum,
    validate: bool,
) -> *mut pgrx::pg_sys::bytea {
    let relopt_kind = RELOPT_KIND.get().copied().expect("init is not called");
    let rdopts = unsafe {
        pgrx::pg_sys::build_reloptions(
            reloptions,
            validate,
            relopt_kind,
            size_of::<Reloption>(),
            TABLE.as_ptr(),
            TABLE.len() as _,
        )
    };
    rdopts as *mut pgrx::pg_sys::bytea
}

#[pgrx::pg_guard]
pub unsafe extern "C-unwind" fn amcostestimate(
    root: *mut pgrx::pg_sys::PlannerInfo,
    path: *mut pgrx::pg_sys::IndexPath,
    loop_count: f64,
    index_startup_cost: *mut pgrx::pg_sys::Cost,
    index_total_cost: *mut pgrx::pg_sys::Cost,
    index_selectivity: *mut pgrx::pg_sys::Selectivity,
    index_correlation: *mut f64,
    index_pages: *mut f64,
) {
    unsafe {
        let index_opt_info = (*path).indexinfo;
        // A runtime index OID cannot select a physical index path safely.
        // Generic plans fall back to scalar sorting unless the target index
        // is known (including a RowExpr with a parameterized query vector).
        let mut target_matches = false;
        // A second ORDER BY key cannot be served by this scan.
        if !(*path).indexorderbys.is_null() && (*(*path).indexorderbys).length == 1 {
            use pgrx::FromDatum;
            let expr =
                pgrx::pg_sys::list_nth((*path).indexorderbys, 0).cast::<pgrx::pg_sys::OpExpr>();
            if (*expr).xpr.type_ == pgrx::pg_sys::NodeTag::T_OpExpr && (*(*expr).args).length == 2 {
                let rhs = pgrx::pg_sys::list_nth((*expr).args, 1).cast::<pgrx::pg_sys::Node>();
                match (*rhs).type_ {
                    pgrx::pg_sys::NodeTag::T_Const => {
                        let rhs = &*rhs.cast::<pgrx::pg_sys::Const>();
                        if rhs.constisnull {
                            target_matches = true;
                        } else {
                            let tuple = pgrx::heap_tuple::PgHeapTuple::<'_, pgrx::AllocatedByRust>::from_datum(rhs.constvalue, false).unwrap();
                            let expected: Option<pgrx::pg_sys::Oid> = tuple
                                .get_by_index(std::num::NonZero::new(2).unwrap())
                                .unwrap();
                            target_matches = expected == Some((*index_opt_info).indexoid);
                        }
                    }
                    pgrx::pg_sys::NodeTag::T_RowExpr => {
                        let row = &*rhs.cast::<pgrx::pg_sys::RowExpr>();
                        if !row.args.is_null() && (*row.args).length == 2 {
                            let target =
                                pgrx::pg_sys::list_nth(row.args, 1).cast::<pgrx::pg_sys::Const>();
                            if (*target).xpr.type_ == pgrx::pg_sys::NodeTag::T_Const
                                && !(*target).constisnull
                            {
                                let expected =
                                    pgrx::pg_sys::Oid::from_datum((*target).constvalue, false);
                                target_matches = expected == Some((*index_opt_info).indexoid);
                            }
                        }
                    }
                    pgrx::pg_sys::NodeTag::T_FuncExpr => {
                        // to_bm25query() is STRICT around ROW(), so PostgreSQL
                        // does not inline it; accept a constant target index.
                        let func = &*rhs.cast::<pgrx::pg_sys::FuncExpr>();
                        let name = pgrx::pg_sys::get_func_name(func.funcid);
                        if !name.is_null()
                            && CStr::from_ptr(name) == c"to_bm25query"
                            && !func.args.is_null()
                            && (*func.args).length == 2
                        {
                            let target =
                                pgrx::pg_sys::list_nth(func.args, 1).cast::<pgrx::pg_sys::Const>();
                            if (*target).xpr.type_ == pgrx::pg_sys::NodeTag::T_Const
                                && !(*target).constisnull
                            {
                                let expected =
                                    pgrx::pg_sys::Oid::from_datum((*target).constvalue, false);
                                target_matches = expected == Some((*index_opt_info).indexoid);
                            }
                        }
                    }
                    _ => (),
                }
            }
        }
        // do not use index, if there are no orderbys or clauses
        if (*path).indexorderbys.is_null() || !target_matches || !gucs::bm25_enable_scan() {
            #[cfg(any(feature = "pg18", feature = "pg19"))]
            {
                (*path).path.disabled_nodes = 2;
            }
            *index_startup_cost = f64::INFINITY;
            *index_total_cost = f64::INFINITY;
            *index_selectivity = 0.0;
            *index_correlation = 0.0;
            *index_pages = 1.0;
            return;
        }
        // Keep PostgreSQL's CPU, tablespace and repeated-scan cache estimates,
        // but posting chains are sequential I/O. Until term-level planner
        // statistics exist, charge a conservative full-index sequential read.
        // This is a heuristic, not a measured WAND/posting selectivity model.
        let mut costs = pgrx::pg_sys::GenericCosts::default();
        pgrx::pg_sys::genericcostestimate(root, path, loop_count, &mut costs);
        let mut random = 0.0;
        let mut sequential = 0.0;
        pgrx::pg_sys::get_tablespace_page_costs(
            (*index_opt_info).reltablespace,
            &mut random,
            &mut sequential,
        );
        let work = (costs.indexTotalCost - costs.numIndexPages * (random - sequential))
            .max(costs.indexStartupCost);
        *index_startup_cost = work;
        *index_total_cost = work;
        // A scan now continues through all candidates; its batch size must
        // never pretend to reduce SQL selectivity or the logical row count.
        *index_selectivity = costs.indexSelectivity;
        *index_correlation = 0.0;
        *index_pages = costs.numIndexPages;
    }
}

#[pgrx::pg_guard]
pub unsafe extern "C-unwind" fn aminsert(
    index_relation: pgrx::pg_sys::Relation,
    values: *mut Datum,
    is_null: *mut bool,
    heap_tid: pgrx::pg_sys::ItemPointer,
    _heap_relation: pgrx::pg_sys::Relation,
    _check_unique: pgrx::pg_sys::IndexUniqueCheck::Type,
    _index_unchanged: bool,
    _index_info: *mut pgrx::pg_sys::IndexInfo,
) -> bool {
    let index = unsafe { PostgresRelation::new(index_relation) };
    let seed = bm25::seed::seed(&index);
    let value = unsafe { (!is_null.add(0).read()).then_some(values.add(0).read()) };
    let ctid = unsafe { heap_tid.read() };
    let document = 'block: {
        use pgrx::datum::FromDatum;
        let Some(datum) = value else {
            break 'block None;
        };
        if datum.is_null() {
            break 'block None;
        }
        let vector = unsafe { TsVectorInput::from_datum(datum, false).unwrap() };
        Some(cast_tsvector_to_document(&seed, vector.as_borrowed()))
    };
    bm25::insert(
        &index,
        &document.unwrap_or_else(bm25::vector::Document::null),
        ctid_to_key(ctid),
    );
    false
}

#[pgrx::pg_guard]
pub unsafe extern "C-unwind" fn ambulkdelete(
    info: *mut pgrx::pg_sys::IndexVacuumInfo,
    stats: *mut pgrx::pg_sys::IndexBulkDeleteResult,
    callback: pgrx::pg_sys::IndexBulkDeleteCallback,
    callback_state: *mut std::os::raw::c_void,
) -> *mut pgrx::pg_sys::IndexBulkDeleteResult {
    use pgrx::pg_sys::ffi::pg_guard_ffi_boundary;
    let mut stats = stats;
    if stats.is_null() {
        stats = unsafe {
            pgrx::pg_sys::palloc0(size_of::<pgrx::pg_sys::IndexBulkDeleteResult>()).cast()
        };
    }
    let index = unsafe { PostgresRelation::new((*info).index) };
    let check = || unsafe {
        #[cfg(any(feature = "pg14", feature = "pg15", feature = "pg16", feature = "pg17"))]
        pgrx::pg_sys::vacuum_delay_point();
        #[cfg(any(feature = "pg18", feature = "pg19"))]
        pgrx::pg_sys::vacuum_delay_point(false);
    };
    let callback = callback.expect("null function pointer");
    let callback = |key: [u16; 3]| {
        let mut ctid = key_to_ctid(key);
        #[allow(ffi_unwind_calls, reason = "protected by pg_guard_ffi_boundary")]
        unsafe {
            pg_guard_ffi_boundary(|| callback(&mut ctid, callback_state))
        }
    };
    let removed = bm25::bulkdelete(&index, check, callback);
    unsafe {
        (*stats).tuples_removed += removed as f64;
    }
    stats
}

#[pgrx::pg_guard]
pub unsafe extern "C-unwind" fn ambeginscan(
    index_relation: pgrx::pg_sys::Relation,
    n_keys: std::os::raw::c_int,
    n_orderbys: std::os::raw::c_int,
) -> pgrx::pg_sys::IndexScanDesc {
    use pgrx::memcxt::PgMemoryContexts::CurrentMemoryContext;

    let scan = unsafe { pgrx::pg_sys::RelationGetIndexScan(index_relation, n_keys, n_orderbys) };
    let scanner: Scanner = Scanner {
        hack: None,
        pending: None,
        scanning: Vec::new().into_iter(),
        prepared: None,
        after: None,
        batch: 0,
        exhausted: false,
        prefilter: false,
    };
    unsafe {
        (*scan).opaque = CurrentMemoryContext.leak_and_drop_on_delete(scanner).cast();
    }
    scan
}

#[pgrx::pg_guard]
pub unsafe extern "C-unwind" fn amrescan(
    scan: pgrx::pg_sys::IndexScanDesc,
    keys: pgrx::pg_sys::ScanKey,
    _n_keys: std::os::raw::c_int,
    orderbys: pgrx::pg_sys::ScanKey,
    _n_orderbys: std::os::raw::c_int,
) {
    unsafe {
        if !keys.is_null() && (*scan).numberOfKeys > 0 {
            std::ptr::copy(keys, (*scan).keyData, (*scan).numberOfKeys as _);
        }
        if !orderbys.is_null() && (*scan).numberOfOrderBys > 0 {
            std::ptr::copy(orderbys, (*scan).orderByData, (*scan).numberOfOrderBys as _);
        }
        if (*scan).numberOfOrderBys == 0 && (*scan).numberOfKeys == 0 {
            pgrx::error!(
                "vector search with no WHERE clause and no ORDER BY clause is not supported"
            );
        }
        let scanner = &mut *(*scan).opaque.cast::<Scanner>();
        scanner.pending = None;
        scanner.prepared = None;
        scanner.after = None;
        scanner.exhausted = false;
        scanner.scanning = Vec::new().into_iter();
        let index = PostgresRelation::new((*scan).indexRelation);
        let options = SearchOptions {
            limit: gucs::bm25_limit((*scan).indexRelation),
            prefilter: gucs::bm25_prefilter((*scan).indexRelation),
        };
        scanner.pending = {
            let mut builder = DefaultBuilder::new(index, ());
            for i in 0..(*scan).numberOfOrderBys {
                let data = (*scan).orderByData.add(i as usize);
                let value = (*data).sk_argument;
                let is_null = ((*data).sk_flags & pgrx::pg_sys::SK_ISNULL as i32) != 0;
                builder.add((*data).sk_strategy, (!is_null).then_some(value));
            }
            for i in 0..(*scan).numberOfKeys {
                let data = (*scan).keyData.add(i as usize);
                let value = (*data).sk_argument;
                let is_null = ((*data).sk_flags & pgrx::pg_sys::SK_ISNULL as i32) != 0;
                builder.add((*data).sk_strategy, (!is_null).then_some(value));
            }
            Some((builder, options))
        };
    }
}

#[pgrx::pg_guard]
pub unsafe extern "C-unwind" fn amgettuple(
    scan: pgrx::pg_sys::IndexScanDesc,
    direction: pgrx::pg_sys::ScanDirection::Type,
) -> bool {
    if direction != pgrx::pg_sys::ScanDirection::ForwardScanDirection {
        pgrx::error!("vector search without a forward scan direction is not supported");
    }
    // https://www.postgresql.org/docs/current/index-locking.html
    // If heap entries referenced physical pointers are deleted before
    // they are consumed by PostgreSQL, PostgreSQL will received wrong
    // physical pointers: no rows or irreverent rows are referenced.
    if unsafe { (*(*scan).xs_snapshot).snapshot_type } != pgrx::pg_sys::SnapshotType::SNAPSHOT_MVCC
    {
        pgrx::error!("scanning with a non-MVCC-compliant snapshot is not supported");
    }
    let scanner = unsafe { (*scan).opaque.cast::<Scanner>().as_mut().unwrap_unchecked() };
    let index = unsafe { PostgresRelation::new((*scan).indexRelation) };
    if let Some((builder, options)) = scanner.pending.take() {
        scanner.prepared = Some(builder.prepare(&index));
        scanner.batch = if options.limit == 0 {
            100
        } else {
            options.limit as usize
        };
        scanner.prefilter = options.prefilter;
        if let Some(hack) = scanner.hack {
            // Volatile predicates must run only in PostgreSQL's executor.
            if unsafe {
                pgrx::pg_sys::contain_volatile_functions((*(*hack.as_ptr()).ss.ps.plan).qual.cast())
            } {
                scanner.prefilter = false;
            }
        }
    }
    loop {
        if let Some((score, key)) = scanner.scanning.next() {
            scanner.after = Some((score, key));
            unsafe {
                (*scan).xs_heaptid = key_to_ctid(key);
                (*scan).xs_recheck = false;
                (*scan).xs_recheckorderby = false;
            }
            return true;
        }
        if scanner.exhausted {
            return false;
        }
        let prepared = scanner.prepared.as_ref().expect("uninitialized scan");
        let hack = scanner.hack.map_or(std::ptr::null_mut(), NonNull::as_ptr);
        // PostgreSQL resources stay on the stack, including on ERROR cleanup.
        let mut fetcher = LazyCell::new(|| unsafe {
            HeapFetcher::new(
                (*scan).indexRelation,
                (*scan).heapRelation,
                (*scan).xs_snapshot,
                (*scan).xs_heapfetch,
                hack,
            )
        });
        let mut results = bm25::search(
            &index,
            std::num::NonZero::new(scanner.batch + 1).unwrap(),
            prepared,
            scanner.after,
            |pointer| {
                if !scanner.prefilter {
                    return true;
                }
                fetcher
                    .fetch(pointer)
                    .is_some_and(|mut tuple| tuple.filter())
            },
            || {
                pgrx::pg_sys::check_for_interrupts!();
            },
            crate::index::storage::reader(),
        );
        drop(fetcher);
        scanner.exhausted = results.len() <= scanner.batch;
        results.truncate(scanner.batch);
        scanner.scanning = results
            .into_iter()
            .map(|(score, key)| (score.0.to_f64(), key))
            .collect::<Vec<_>>()
            .into_iter();
        scanner.batch = scanner.batch.saturating_mul(2).min(u32::MAX as usize);
    }
}

#[pgrx::pg_guard]
pub unsafe extern "C-unwind" fn amendscan(scan: pgrx::pg_sys::IndexScanDesc) {
    let scanner = unsafe { &mut *(*scan).opaque.cast::<Scanner>() };
    scanner.pending = None;
    scanner.prepared = None;
    scanner.scanning = Vec::new().into_iter();
}

// Only Rust-owned query data and materialized results may live in this state.
// Its destructor can run after PostgreSQL has already released all resources.
pub struct Scanner {
    pub hack: Option<NonNull<pgrx::pg_sys::IndexScanState>>,
    pending: Option<(DefaultBuilder, SearchOptions)>,
    prepared: Option<std::rc::Rc<bm25::Prepared>>,
    after: Option<(f64, [u16; 3])>,
    batch: usize,
    exhausted: bool,
    prefilter: bool,
    scanning: std::vec::IntoIter<(f64, [u16; 3])>,
}
