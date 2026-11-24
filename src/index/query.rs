// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU Affero General Public License v3 (AGPLv3).
// See <https://www.gnu.org/licenses/agpl-3.0.html> for details.
//
// Copyright (c) 2025-2026 TensorChord Inc.
// Copyright (c) 2026 PGSTY PTE. LTD.
// Maintained by Ruohang Feng @ PGSTY (rh@vonng.com)

use crate::index::storage::PostgresRelation;
use bm25::vector::Query;
use index::lru::Lru;
use pgrx::pg_sys::{self, Oid};
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

// Statistics do not depend on the role; permission checks do.
type Key = (Oid, Query);
#[derive(Hash, PartialEq, Eq)]
struct Authority {
    index: Oid,
    role: Oid,
    row_security: bool,
    no_force_rls: bool,
}

#[derive(Default)]
struct Cache {
    entries: RefCell<Lru<Key, bm25::Prepared>>,
    seeds: RefCell<HashMap<Authority, [u8; 32]>>,
}

impl Cache {
    // Rust memory is outside PostgreSQL's accounting: bound it by work_mem,
    // softly. Hits refresh an entry; statistics still held by a scan and the
    // most recently used queries are never evicted, so a scan and scalar
    // scoring of the same query share one preparation. Other queries are
    // evicted least recently used first and prepared again on their next use.
    fn insert(&self, key: Key, prepared: Rc<bm25::Prepared>) {
        let budget = (unsafe { pg_sys::work_mem } as usize).saturating_mul(1024);
        let size = prepared.approximate_size();
        self.entries
            .borrow_mut()
            .insert(key, prepared, size, budget);
    }
}

thread_local! {
    static QUERIES: RefCell<HashMap<usize, Rc<Cache>>> = RefCell::default();
    static ACTIVE: RefCell<Option<Rc<Cache>>> = const { RefCell::new(None) };
}

/// Bound to es_query_cxt, including ERROR cleanup. Owns Rust data only.
pub struct Lease(usize);
impl Drop for Lease {
    fn drop(&mut self) {
        QUERIES.with_borrow_mut(|q| {
            q.remove(&self.0);
        });
    }
}
pub struct Active(Option<Rc<Cache>>);
impl Drop for Active {
    fn drop(&mut self) {
        ACTIVE.with_borrow_mut(|a| *a = self.0.take());
    }
}
pub fn begin(desc: *mut pg_sys::QueryDesc) -> Lease {
    QUERIES.with_borrow_mut(|q| {
        q.insert(desc as usize, Rc::default());
    });
    Lease(desc as usize)
}
pub fn enter(desc: *mut pg_sys::QueryDesc) -> Active {
    let current = QUERIES.with_borrow(|q| q.get(&(desc as usize)).cloned());
    Active(ACTIVE.with_borrow_mut(|a| std::mem::replace(a, current)))
}

/// Checks that the current role may read the index's corpus statistics, once
/// per execution, index and role.
pub fn seed(oid: Oid) -> [u8; 32] {
    let authority = unsafe {
        Authority {
            index: oid,
            role: pg_sys::GetUserId(),
            row_security: pg_sys::row_security,
            no_force_rls: pg_sys::InNoForceRLSOperation(),
        }
    };
    let active = ACTIVE.with_borrow(Clone::clone);
    if let Some(cache) = &active {
        if let Some(seed) = cache.seeds.borrow().get(&authority) {
            return *seed;
        }
    }
    let seed = crate::index::operators::load_seed(oid);
    if let Some(cache) = active {
        cache.seeds.borrow_mut().insert(authority, seed);
    }
    seed
}

pub fn prepare(
    oid: Oid,
    query: &Query,
    scan: Option<&PostgresRelation<bm25::Opaque>>,
) -> Rc<bm25::Prepared> {
    let active = ACTIVE.with_borrow(Clone::clone);
    let key = (oid, query.clone());
    if let Some(cache) = &active {
        let hit = cache.entries.borrow_mut().get(&key);
        if let Some(prepared) = hit {
            if scan.is_some_and(|index| {
                prepared.prepare_scan(index, crate::index::storage::reader(), || {
                    pgrx::check_for_interrupts()
                })
            }) {
                cache.insert(key, prepared.clone());
            }
            return prepared;
        }
    }
    let relation;
    let borrowed;
    let (index, purpose) = match scan {
        Some(index) => (index, bm25::Purpose::Scan),
        None => {
            // The seed lookup retained the table and index locks for this transaction.
            relation = crate::index::operators::Index::open(oid, pg_sys::NoLock as _);
            borrowed = unsafe { PostgresRelation::new(relation.raw()) };
            (&borrowed, bm25::Purpose::Score)
        }
    };
    let prepared = Rc::new(bm25::Prepared::collect(
        index,
        query,
        purpose,
        || {
            pgrx::check_for_interrupts();
        },
        crate::index::storage::reader(),
    ));
    if let Some(cache) = active {
        cache.insert(key, prepared.clone());
    }
    prepared
}
