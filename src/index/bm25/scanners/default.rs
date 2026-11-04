// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU Affero General Public License v3 (AGPLv3).
// See <https://www.gnu.org/licenses/agpl-3.0.html> for details.
//
// Copyright (c) 2025-2026 TensorChord Inc.
// Copyright (c) 2026 PGSTY PTE. LTD.
// Maintained by Ruohang Feng @ PGSTY (rh@vonng.com)

use crate::datatype::memory_tsvector::TsVectorOutput;
use crate::datatype::tsvector::cast_tsvector_to_query;
use bm25::vector::Query;
use index::relation::{Page, RelationId, RelationRead};
use pgrx::heap_tuple::PgHeapTuple;
use pgrx::pg_sys::Oid;
use std::num::NonZero;

pub struct DefaultBuilder {
    oid: Oid,
    seed: [u8; 32],
    orderbys: Vec<Option<Query>>,
}

impl DefaultBuilder {
    pub fn new<R>(index: R, (): ()) -> Self
    where
        R: RelationId + RelationRead,
        R::Page: Page<Opaque = bm25::Opaque>,
    {
        let oid = Oid::from_u32(index.id());
        crate::index::operators::check_index_permissions(oid);
        let seed = bm25::seed::seed(&index);
        Self {
            oid,
            seed,
            orderbys: Vec::new(),
        }
    }

    pub unsafe fn add(&mut self, strategy: u16, value: Option<pgrx::pg_sys::Datum>) {
        match strategy {
            1 => {
                let document = 'block: {
                    use pgrx::datum::FromDatum;
                    let Some(datum) = value else {
                        break 'block None;
                    };
                    if datum.is_null() {
                        break 'block None;
                    }
                    let rhs = unsafe {
                        PgHeapTuple::<'_, pgrx::AllocatedByRust>::from_datum(datum, false).unwrap()
                    };
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
                    if index != self.oid {
                        pgrx::error!(
                            "expected index {index}, but got index {} for scan",
                            self.oid
                        );
                    }
                    Some(cast_tsvector_to_query(&self.seed, vector.as_borrowed()))
                };
                self.orderbys.push(document);
            }
            _ => unreachable!(),
        }
    }

    pub fn prepare(
        self,
        index: &crate::index::storage::PostgresRelation<bm25::Opaque>,
    ) -> std::rc::Rc<bm25::Prepared> {
        let mut vector = None;
        for orderby in self.orderbys.into_iter().flatten() {
            if vector.replace(orderby).is_some() {
                pgrx::error!("vector search with multiple vectors is not supported");
            }
        }
        // A NULL ordering expression is constant for every row; any order is
        // valid, but it still has to return every indexed row.
        let query = vector.unwrap_or_else(|| Query::new(Vec::new()));
        crate::index::query::prepare(index, self.oid, &query)
    }
}
