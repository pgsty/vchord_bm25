// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU Affero General Public License v3 (AGPLv3).
// See <https://www.gnu.org/licenses/agpl-3.0.html> for details.
//
// Copyright (c) 2026 PGSTY PTE. LTD.
// Maintained by Ruohang Feng @ PGSTY (rh@vonng.com)

use crate::index::bm25::am::Reloption;
use pgrx::pg_sys::{self, Node, NodeTag, Oid};

struct ColumnAccess {
    table: Oid,
    role: Oid,
    saw_column: bool,
}

impl ColumnAccess {
    fn column(&mut self, number: pg_sys::AttrNumber) -> bool {
        self.saw_column = true;
        number > 0
            && unsafe {
                pg_sys::pg_attribute_aclcheck(
                    self.table,
                    number,
                    self.role,
                    pg_sys::ACL_SELECT as _,
                ) == pg_sys::AclResult::ACLCHECK_OK
            }
    }

    fn executable(&self, function: Oid) -> bool {
        unsafe {
            #[cfg(feature = "pg15")]
            let result = pg_sys::pg_proc_aclcheck(function, self.role, pg_sys::ACL_EXECUTE as _);
            #[cfg(not(feature = "pg15"))]
            let result = pg_sys::object_aclcheck(
                pg_sys::ProcedureRelationId,
                function,
                self.role,
                pg_sys::ACL_EXECUTE as _,
            );
            result == pg_sys::AclResult::ACLCHECK_OK
        }
    }

    unsafe fn list(&mut self, nodes: *mut pg_sys::List) -> bool {
        unsafe {
            nodes.is_null()
                || (0..(*nodes).length).all(|i| self.expression(pg_sys::list_nth(nodes, i).cast()))
        }
    }

    unsafe fn expression(&mut self, node: *mut Node) -> bool {
        unsafe {
            match (*node).type_ {
                NodeTag::T_List => self.list(node.cast()),
                NodeTag::T_Var => {
                    let var = &*node.cast::<pg_sys::Var>();
                    var.varno == 1 && var.varlevelsup == 0 && self.column(var.varattno)
                }
                NodeTag::T_Const => true,
                NodeTag::T_FuncExpr => {
                    let expr = &*node.cast::<pg_sys::FuncExpr>();
                    self.executable(expr.funcid) && self.list(expr.args)
                }
                NodeTag::T_OpExpr | NodeTag::T_DistinctExpr | NodeTag::T_NullIfExpr => {
                    let expr = &*node.cast::<pg_sys::OpExpr>();
                    self.executable(pg_sys::get_opcode(expr.opno)) && self.list(expr.args)
                }
                NodeTag::T_BoolExpr => self.list((*node.cast::<pg_sys::BoolExpr>()).args),
                NodeTag::T_NullTest => {
                    self.expression((*node.cast::<pg_sys::NullTest>()).arg.cast())
                }
                NodeTag::T_BooleanTest => {
                    self.expression((*node.cast::<pg_sys::BooleanTest>()).arg.cast())
                }
                NodeTag::T_RelabelType => {
                    self.expression((*node.cast::<pg_sys::RelabelType>()).arg.cast())
                }
                NodeTag::T_CollateExpr => {
                    self.expression((*node.cast::<pg_sys::CollateExpr>()).arg.cast())
                }
                _ => false,
            }
        }
    }

    unsafe fn index(&mut self, index: pg_sys::Relation) -> bool {
        unsafe {
            let info = &*(*index).rd_index;
            let keys = info.indkey.values.as_slice(info.indnkeyatts as usize);
            if !keys.iter().all(|&n| n == 0 || self.column(n)) {
                return false;
            }
            // Inspect catalog expressions before constant folding or inlining can erase dependencies.
            for attr in [
                pg_sys::Anum_pg_index_indexprs,
                pg_sys::Anum_pg_index_indpred,
            ] {
                let mut null = false;
                let value = pg_sys::SysCacheGetAttr(
                    pg_sys::SysCacheIdentifier::INDEXRELID as _,
                    (*index).rd_indextuple,
                    attr as _,
                    &mut null,
                );
                if !null {
                    let expr = pg_sys::stringToNode(pg_sys::text_to_cstring(value.cast_mut_ptr()));
                    if !self.expression(expr.cast()) {
                        return false;
                    }
                }
            }
            self.saw_column
        }
    }
}

pub(super) unsafe fn check(index: pg_sys::Relation) {
    unsafe {
        let table = (*(*index).rd_index).indrelid;
        let role = pg_sys::GetUserId();
        if pg_sys::pg_class_aclcheck(table, role, pg_sys::ACL_SELECT as _)
            != pg_sys::AclResult::ACLCHECK_OK
            && !(ColumnAccess {
                table,
                role,
                saw_column: false,
            })
            .index(index)
        {
            pg_sys::aclcheck_error(
                pg_sys::AclResult::ACLCHECK_NO_PRIV,
                pg_sys::ObjectType::OBJECT_TABLE,
                pg_sys::get_rel_name(table),
            );
        }
        let shared = Reloption::shared_corpus_stats((*index).rd_options.cast());
        // PostgreSQL raises its own row_security=off error on an opted-in index.
        if pg_sys::check_enable_rls(table, pg_sys::InvalidOid, !shared)
            == pg_sys::CheckEnableRlsResult::RLS_ENABLED as i32
            && !shared
        {
            pgrx::ereport!(
                ERROR,
                pgrx::PgSqlErrorCode::ERRCODE_INSUFFICIENT_PRIVILEGE,
                "bm25 cannot expose global corpus statistics under row-level security"
            );
        }
    }
}
