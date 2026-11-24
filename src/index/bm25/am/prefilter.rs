// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU Affero General Public License v3 (AGPLv3).
// See <https://www.gnu.org/licenses/agpl-3.0.html> for details.
//
// Copyright (c) 2025-2026 TensorChord Inc.
// Copyright (c) 2026 PGSTY PTE. LTD.
// Maintained by Ruohang Feng @ PGSTY (rh@vonng.com)

use pgrx::pg_sys::{self, NodeTag, contain_volatile_functions as volatile};
use std::ptr::NonNull;

pub(super) unsafe fn safe(scan: NonNull<pg_sys::IndexScanState>) -> bool {
    unsafe {
        let ps = &mut (*scan.as_ptr()).ss.ps;
        let statement = (*ps.state).es_plannedstmt;
        if (*statement).dependsOnRole
            || (*statement).hasModifyingCTE
            || !(*statement).rowMarks.is_null()
        {
            return false;
        }
        let rtable = (*statement).rtable;
        if rtable.is_null()
            || (0..(*rtable).length).any(|i| {
                let entry = pg_sys::list_nth(rtable, i).cast::<pg_sys::RangeTblEntry>();
                (*entry).security_barrier || !(*entry).securityQuals.is_null()
            })
        {
            return false;
        }
        let qual = (*ps.plan).qual.cast();
        !volatile(qual)
            && (!pg_sys::contain_subplans(qual)
                || !pg_sys::planstate_tree_walker(ps, Some(unsafe_subplan), std::ptr::null_mut()))
    }
}

#[pgrx::pg_guard]
unsafe extern "C-unwind" fn unsafe_subplan(
    node: *mut pg_sys::PlanState,
    ctx: *mut std::ffi::c_void,
) -> bool {
    unsafe {
        if node.is_null() {
            return false;
        }
        let plan = (*node).plan;
        if plan.is_null() || volatile((*plan).qual.cast()) || volatile((*plan).targetlist.cast()) {
            return true;
        }
        let heap_scan = |scan: *mut pg_sys::ScanState| {
            let rel = (*scan).ss_currentRelation;
            !rel.is_null()
                && (*(*rel).rd_rel).relam == pg_sys::HEAP_TABLE_AM_OID
                && !(*(*rel).rd_rel).relrowsecurity
        };
        let btree = |rel: pg_sys::Relation| {
            !rel.is_null() && (*(*rel).rd_rel).relam == pg_sys::BTREE_AM_OID
        };
        let safe = match (*plan).type_ {
            NodeTag::T_Result => !volatile((*plan.cast::<pg_sys::Result>()).resconstantqual.cast()),
            NodeTag::T_SeqScan => heap_scan(node.cast()),
            NodeTag::T_IndexScan => {
                let p = &*plan.cast::<pg_sys::IndexScan>();
                heap_scan(node.cast())
                    && btree((*node.cast::<pg_sys::IndexScanState>()).iss_RelationDesc)
                    && !volatile(p.indexqual.cast())
                    && !volatile(p.indexorderby.cast())
                    && !volatile(p.indexqualorig.cast())
                    && !volatile(p.indexorderbyorig.cast())
            }
            NodeTag::T_IndexOnlyScan => {
                let p = &*plan.cast::<pg_sys::IndexOnlyScan>();
                heap_scan(node.cast())
                    && btree((*node.cast::<pg_sys::IndexOnlyScanState>()).ioss_RelationDesc)
                    && !volatile(p.indexqual.cast())
                    && !volatile(p.indexorderby.cast())
                    && !volatile(p.recheckqual.cast())
            }
            // User aggregates may execute transition and final functions hidden in Aggref.
            NodeTag::T_Agg => {
                let agg = &*plan.cast::<pg_sys::Agg>();
                if agg.aggstrategy != pg_sys::AggStrategy::AGG_PLAIN
                    || agg.aggsplit != pg_sys::AggSplit::AGGSPLIT_SIMPLE
                    || !(*plan).qual.is_null()
                    || (*plan).targetlist.is_null()
                    || (*(*plan).targetlist).length != 1
                {
                    return true;
                }
                let target = pg_sys::list_nth((*plan).targetlist, 0).cast::<pg_sys::TargetEntry>();
                let expr = (*target).expr;
                (*expr).type_ == NodeTag::T_Aggref && {
                    let a = &*expr.cast::<pg_sys::Aggref>();
                    a.aggfnoid.to_u32() == pg_sys::F_COUNT_ && a.aggstar && a.aggfilter.is_null()
                }
            }
            _ => false,
        };
        !safe || pg_sys::planstate_tree_walker(node, Some(unsafe_subplan), ctx)
    }
}
