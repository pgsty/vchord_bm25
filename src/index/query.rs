// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU Affero General Public License v3 (AGPLv3).
// See <https://www.gnu.org/licenses/agpl-3.0.html> for details.
//
// Copyright (c) 2025-2026 TensorChord Inc.
// Copyright (c) 2026 PGSTY PTE. LTD.
// Maintained by Ruohang Feng @ PGSTY (rh@vonng.com)

use crate::index::storage::PostgresRelation;
use bm25::vector::Query;
use pgrx::pg_sys::{self, Oid};
use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet, VecDeque};
use std::rc::Rc;

// Statistics do not depend on the role; permission checks do.
type Key = (Oid, Query);
#[derive(Default)]
struct Cache {
    entries: RefCell<HashMap<Key, Rc<bm25::Prepared>>>,
    order: RefCell<VecDeque<Key>>,
    bytes: Cell<usize>,
    checked: RefCell<HashSet<(Oid, Oid)>>,
}

impl Cache {
    // Rust memory is outside PostgreSQL's accounting: bound it by work_mem,
    // evicting the oldest query. An evicted query is prepared again on its
    // next use; a scan still holding it keeps its own reference.
    fn insert(&self, key: Key, prepared: Rc<bm25::Prepared>) {
        let budget = (unsafe { pg_sys::work_mem } as usize).saturating_mul(1024);
        let size = prepared.approximate_size();
        let mut entries = self.entries.borrow_mut();
        let mut order = self.order.borrow_mut();
        while self.bytes.get() + size > budget {
            let Some(old) = order.pop_front() else {
                break;
            };
            if let Some(evicted) = entries.remove(&old) {
                self.bytes
                    .set(self.bytes.get() - evicted.approximate_size());
            }
        }
        self.bytes.set(self.bytes.get() + size);
        order.push_back(key.clone());
        entries.insert(key, prepared);
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

pub fn prepare(
    index: &PostgresRelation<bm25::Opaque>,
    oid: Oid,
    query: &Query,
) -> Rc<bm25::Prepared> {
    let role = unsafe { pg_sys::GetUserId() };
    let active = ACTIVE.with_borrow(Clone::clone);
    // Record a role only after its check passed: an error caught by a
    // PL/pgSQL handler must not let a retry in this execution skip it.
    match &active {
        Some(cache) if cache.checked.borrow().contains(&(oid, role)) => (),
        Some(cache) => {
            crate::index::operators::check_index_permissions(oid);
            cache.checked.borrow_mut().insert((oid, role));
        }
        None => crate::index::operators::check_index_permissions(oid),
    }
    let key = (oid, query.clone());
    if let Some(cache) = &active {
        if let Some(prepared) = cache.entries.borrow().get(&key) {
            return prepared.clone();
        }
    }
    let prepared = Rc::new(bm25::Prepared::collect(
        index,
        query,
        || {
            pg_sys::check_for_interrupts!();
        },
        crate::index::storage::reader(),
    ));
    if let Some(cache) = active {
        cache.insert(key, prepared.clone());
    }
    prepared
}
