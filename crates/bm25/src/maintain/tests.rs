// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU Affero General Public License v3 (AGPLv3).
// See <https://www.gnu.org/licenses/agpl-3.0.html> for details.
// Copyright (c) 2026 PGSTY PTE. LTD.

// Deterministic recovery tests for the real maintenance implementation.
// Each guard commit is a WAL record; recovery may stop at any record boundary.
// FSM hints deliberately survive independently of these records.

use super::*;
use index::relation::{PageGuard, Relation, RelationReadTypes, RelationWriteTypes};
use std::cell::{Cell, RefCell};
use std::ops::{Deref, DerefMut};

#[derive(Clone, Debug, PartialEq)]
struct TestPage {
    opaque: Opaque,
    items: Vec<Vec<u8>>,
}

impl Page for TestPage {
    type Opaque = Opaque;
    fn get_opaque(&self) -> &Opaque {
        &self.opaque
    }
    fn get_opaque_mut(&mut self) -> &mut Opaque {
        &mut self.opaque
    }
    fn len(&self) -> u16 {
        self.items.len() as u16
    }
    fn get(&self, i: u16) -> Option<&[u8]> {
        self.items.get(usize::from(i) - 1).map(Vec::as_slice)
    }
    fn get_mut(&mut self, i: u16) -> Option<&mut [u8]> {
        self.items
            .get_mut(usize::from(i) - 1)
            .map(Vec::as_mut_slice)
    }
    fn alloc(&mut self, data: &[u8]) -> Option<u16> {
        self.items.push(data.to_vec());
        Some(self.len())
    }
    fn free(&mut self, _: u16) {
        unimplemented!()
    }
    fn freespace(&self) -> u16 {
        7000
    }
    fn clear(&mut self, opaque: Opaque) {
        self.items.clear();
        self.opaque = opaque;
    }
    fn is_new(&self) -> bool {
        false
    }
}

struct TestRelation {
    pages: RefCell<Vec<TestPage>>,
    // Positive = readers, -1 = writer. Detect incorrect lock ordering rather
    // than hanging the unit test if a writer is requested behind a reader.
    locks: RefCell<Vec<i32>>,
    wal: RefCell<Vec<Vec<TestPage>>>,
    fsm: RefCell<Vec<u32>>,
    check_insert_locks: Cell<bool>,
    selected_fsm: Cell<Option<u32>>,
    fail_tail_locks: Cell<usize>,
    publish_on_tail_read: Cell<bool>,
}

impl TestRelation {
    fn new(pages: Vec<TestPage>) -> Self {
        Self {
            locks: RefCell::new(vec![0; pages.len()]),
            wal: RefCell::new(vec![pages.clone()]),
            pages: RefCell::new(pages),
            fsm: RefCell::default(),
            check_insert_locks: Cell::new(false),
            selected_fsm: Cell::new(None),
            fail_tail_locks: Cell::new(0),
            publish_on_tail_read: Cell::new(false),
        }
    }
    fn commit(&self, updates: &[(u32, &TestPage)]) {
        let mut pages = self.pages.borrow_mut();
        let changed = updates
            .iter()
            .any(|(id, page)| pages[*id as usize] != **page);
        if changed {
            for &(id, page) in updates {
                pages[id as usize] = page.clone();
            }
            self.wal.borrow_mut().push(pages.clone());
        }
    }
}

struct Read<'a> {
    relation: &'a TestRelation,
    id: u32,
    page: TestPage,
}
impl PageGuard for Read<'_> {
    fn id(&self) -> u32 {
        self.id
    }
}
impl Deref for Read<'_> {
    type Target = TestPage;
    fn deref(&self) -> &TestPage {
        &self.page
    }
}
impl Drop for Read<'_> {
    fn drop(&mut self) {
        self.relation.locks.borrow_mut()[self.id as usize] -= 1;
        // GetFreeIndexPage already consumed the hint before sweep took its
        // read lock. The allocator now obtains its exclusive page lock.
        if self.relation.selected_fsm.get() == Some(self.id) {
            self.relation.selected_fsm.set(None);
            self.relation.write(self.id).clear(Opaque {
                next: u32::MAX,
                flags: 0,
                generation: 2,
            });
        }
    }
}
struct Write<'a> {
    relation: &'a TestRelation,
    id: u32,
    page: TestPage,
}
impl PageGuard for Write<'_> {
    fn id(&self) -> u32 {
        self.id
    }
}
impl Deref for Write<'_> {
    type Target = TestPage;
    fn deref(&self) -> &TestPage {
        &self.page
    }
}
impl DerefMut for Write<'_> {
    fn deref_mut(&mut self) -> &mut TestPage {
        &mut self.page
    }
}
impl Drop for Write<'_> {
    fn drop(&mut self) {
        if !std::thread::panicking() {
            self.relation.commit(&[(self.id, &self.page)]);
        }
        self.relation.locks.borrow_mut()[self.id as usize] = 0;
    }
}
impl Relation for TestRelation {
    type Page = TestPage;
}
impl RelationReadTypes for TestRelation {
    type ReadGuard<'a> = Read<'a>;
}
impl RelationRead for TestRelation {
    fn read(&self, id: u32) -> Read<'_> {
        if self.check_insert_locks.get() && id == 4 {
            assert_eq!(
                self.locks.borrow()[1],
                0,
                "waiting for tail while holding jump"
            );
        }
        if id == 4 && self.publish_on_tail_read.replace(false) {
            let mut jump = self.write(1);
            *JumpTuple::deserialize_mut(jump.get_mut(1).unwrap()).generation() += 1;
        }
        assert!(
            self.locks.borrow()[id as usize] >= 0,
            "read behind writer: {id}"
        );
        self.locks.borrow_mut()[id as usize] += 1;
        Read {
            relation: self,
            id,
            page: self.pages.borrow()[id as usize].clone(),
        }
    }
}
impl RelationWriteTypes for TestRelation {
    type WriteGuard<'a> = Write<'a>;
}
impl RelationWrite for TestRelation {
    fn discard(&self, mut guard: Write<'_>) {
        guard.page = self.pages.borrow()[guard.id as usize].clone();
    }
    fn write(&self, id: u32) -> Write<'_> {
        if self.check_insert_locks.get() && id == 4 {
            assert_eq!(
                self.locks.borrow()[1],
                0,
                "waiting for tail while holding jump"
            );
        }
        assert_eq!(
            self.locks.borrow()[id as usize],
            0,
            "write behind lock: {id}"
        );
        self.locks.borrow_mut()[id as usize] = -1;
        Write {
            relation: self,
            id,
            page: self.pages.borrow()[id as usize].clone(),
        }
    }
    fn alloc(&self, opaque: Opaque) -> Write<'_> {
        loop {
            let candidate = self.fsm.borrow_mut().pop();
            let Some(id) = candidate else {
                panic!("test exhausted FSM")
            };
            let mut page = self.write(id);
            if page.get_opaque().is_deleted() {
                page.clear(opaque);
                return page;
            }
        }
    }
    fn try_write(&self, id: u32) -> Option<Write<'_>> {
        if id == 4 && self.fail_tail_locks.get() != 0 {
            self.fail_tail_locks.set(self.fail_tail_locks.get() - 1);
            return None;
        }
        if self.locks.borrow()[id as usize] != 0 {
            return None;
        }
        self.locks.borrow_mut()[id as usize] = -1;
        Some(Write {
            relation: self,
            id,
            page: self.pages.borrow()[id as usize].clone(),
        })
    }
    fn commit_pair<'a>(&'a self, left: Write<'a>, right: Write<'a>) -> Write<'a> {
        self.commit(&[(left.id, &left.page), (right.id, &right.page)]);
        drop(left);
        right
    }
    fn free(&self, mut page: Write<'_>) {
        page.get_opaque_mut().set_deleted();
        self.record_free(page.id());
    }
    fn vacuum(&self) {}
    fn npages(&self) -> u32 {
        self.pages.borrow().len() as u32
    }
    fn record_free(&self, id: u32) {
        self.fsm.borrow_mut().push(id);
    }
}

fn fixture() -> TestRelation {
    let jump = JumpTuple {
        ptr_vectors: 4,
        ptr_vectors_offset: 1,
        tail_hint: 4,
        number_of_documents: 0,
        number_of_nulls: 0,
        sum_of_document_lengths: 0,
        width_1_documents: 0,
        width_0_documents: 0,
        depth_documents: 0,
        start_documents: u32::MAX,
        free_documents: u32::MAX,
        depth_tokens: 0,
        start_tokens: u32::MAX,
        free_tokens: u32::MAX,
        ptr_documents: u32::MAX,
        ptr_tokens: u32::MAX,
        ptr_summaries: u32::MAX,
        ptr_blocks: u32::MAX,
        generation: 2,
        safexid: 123,
        graveyard_vectors: 2,
        graveyard_vectors_end: 4,
        graveyard_documents: u32::MAX,
        graveyard_tokens: u32::MAX,
        graveyard_summaries: u32::MAX,
        graveyard_blocks: u32::MAX,
        graveyard_free_documents: u32::MAX,
        graveyard_free_tokens: u32::MAX,
    };
    let mut pages = (0..5)
        .map(|_| TestPage {
            opaque: Opaque {
                next: u32::MAX,
                flags: 0,
                generation: 1,
            },
            items: Vec::new(),
        })
        .collect::<Vec<_>>();
    pages[1].items.push(jump.serialize());
    pages[0].items.push(
        MetaTuple {
            k1: 1.2,
            b: 0.75,
            ptr_jump: 1,
            seed: [0; 32],
        }
        .serialize(),
    );
    pages[2].opaque.next = 3;
    pages[3].opaque.next = 4;
    TestRelation::new(pages)
}

#[test]
fn stale_fsm_reuse_survives_every_recycle_wal_boundary() {
    let original = fixture();
    assert_eq!(recycle(&original, 1, &|| {}), 2);
    let prefixes = original.wal.borrow().clone();
    for (boundary, pages) in prefixes.into_iter().enumerate() {
        let recovered = TestRelation::new(pages);
        let reusable = (2..=3)
            .filter(|&id| recovered.pages.borrow()[id].opaque.is_deleted())
            .collect::<Vec<_>>();
        // A persisted stale FSM hint may offer any deleted page immediately
        // after recovery, before VACUUM resumes the old graveyard head.
        for &id in &reusable {
            recovered.record_free(id as u32);
            let mut allocated = recovered.alloc(Opaque {
                next: u32::MAX,
                flags: 0,
                generation: 2,
            });
            assert_eq!(allocated.id(), id as u32);
            allocated.alloc(b"new growing document").unwrap();
        }
        recycle(&recovered, 1, &|| {});
        for id in reusable {
            let page = recovered.read(id as u32);
            assert!(
                !page.opaque.is_deleted(),
                "live page {id} recycled again after WAL boundary {boundary}"
            );
            assert_eq!(page.get(1), Some(b"new growing document".as_slice()));
        }
        assert_eq!(
            recovered.pages.borrow()[4],
            original.pages.borrow()[4],
            "retained growing tail must survive"
        );
    }
}

#[test]
fn recycle_rejects_a_reused_graveyard_page_without_modifying_it() {
    let relation = fixture();
    relation.pages.borrow_mut()[2].opaque.generation = 2;
    relation.pages.borrow_mut()[2].items.push(b"live".to_vec());
    let before = relation.pages.borrow().clone();
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        recycle(&relation, 1, &|| {});
    }));
    assert!(result.is_err(), "a live generation must not be recycled");
    assert_eq!(*relation.pages.borrow(), before);
}

#[test]
fn insert_waits_for_tail_without_holding_jump() {
    let relation = fixture();
    relation.check_insert_locks.set(true);
    crate::insert(&relation, &Document::new(Vec::new()), [0, 1, 1], || {});
    assert_eq!(relation.read(4).len(), 2);
}

#[test]
fn sweep_can_republish_an_already_selected_fsm_candidate() {
    let relation = fixture();
    relation.pages.borrow_mut()[2].opaque.set_deleted();
    relation.record_free(2);
    relation.selected_fsm.set(relation.fsm.borrow_mut().pop());
    sweep(&relation, 2, true, &|| {});
    assert!(!relation.read(2).opaque.is_deleted());
    assert!(
        relation.fsm.borrow().contains(&2),
        "a stale hint survives despite the locked sweep"
    );
}

#[test]
fn insert_revalidates_publication_and_retries_without_locks() {
    let relation = fixture();
    relation.check_insert_locks.set(true);
    relation.publish_on_tail_read.set(true);
    relation.fail_tail_locks.set(2);
    let checks = Cell::new(0);
    crate::insert(&relation, &Document::new(Vec::new()), [0, 1, 1], || {
        assert!(relation.locks.borrow().iter().all(|&n| n == 0));
        checks.set(checks.get() + 1);
    });
    assert_eq!(
        checks.get(),
        4,
        "initial attempt, publication change, two contended attempts"
    );
    assert_eq!(relation.read(4).len(), 2, "append exactly once");
}

#[test]
fn nine_multi_term_queries_fit_work_mem_without_recollection() {
    use crate::vector::Query;
    use std::rc::Rc;
    let relation = fixture();
    let mut cache = index::lru::Lru::new();
    let mut collections = 0;
    for _ in 0..10 {
        for key in 1..=9 {
            if cache.get(&key).is_none() {
                let terms = (1..=20)
                    .map(|term| {
                        let mut token = [0; WIDTH];
                        token[0] = key;
                        token[1] = term;
                        token
                    })
                    .collect();
                let prepared = Rc::new(crate::Prepared::collect(
                    &relation,
                    &Query::new(terms),
                    || {},
                    crate::Reader {
                        in_recovery: false,
                        conflict: &|| panic!("unexpected conflict"),
                    },
                ));
                let size = prepared.approximate_size();
                cache.insert(key, prepared, size, 64 * 1024);
                collections += 1;
            }
        }
    }
    println!(
        "nine 20-term queries: {collections} preparations, {} retained bytes",
        cache.bytes()
    );
    assert_eq!(
        collections, 9,
        "the first statistics for each key must remain shared"
    );
    assert!(cache.bytes() <= 64 * 1024);
}
