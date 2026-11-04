// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU Affero General Public License v3 (AGPLv3).
// See <https://www.gnu.org/licenses/agpl-3.0.html> for details.
//
// Copyright (c) 2025-2026 TensorChord Inc.
// Copyright (c) 2026 PGSTY PTE. LTD.
// Maintained by Ruohang Feng @ PGSTY (rh@vonng.com)

use crate::Opaque;
use crate::tuples::Tuple;
use index::relation::{Page, PageGuard, RelationRead, RelationWrite};
use std::collections::VecDeque;
use std::marker::PhantomData;

pub struct TapeWriter<'a, R, T>
where
    R: RelationWrite + 'a,
{
    head: Option<R::WriteGuard<'a>>,
    current: u32,
    first: u32,
    index: &'a R,
    generation: u64,
    _phantom: PhantomData<fn(T) -> T>,
}

impl<'a, R, T> TapeWriter<'a, R, T>
where
    R: RelationWrite + 'a,
    R::Page: Page<Opaque = Opaque>,
{
    pub fn create(index: &'a R, generation: u64) -> Self {
        let head = index.alloc(Opaque {
            next: u32::MAX,
            flags: 0,
            generation,
        });
        let first = head.id();
        Self {
            head: Some(head),
            current: first,
            first,
            index,
            generation,
            _phantom: PhantomData,
        }
    }
    pub fn from_guard(index: &'a R, head: R::WriteGuard<'a>, generation: u64) -> Self {
        let first = head.id();
        Self {
            head: Some(head),
            current: first,
            first,
            index,
            generation,
            _phantom: PhantomData,
        }
    }
    pub fn finish(self) -> u32 {
        self.current
    }
    // Only unpublished output tapes may release their locks mid-stream.
    pub fn release(&mut self) {
        self.head.take();
    }
    pub fn acquire(&mut self) {
        self.head = Some(self.index.write(self.current));
    }
    pub fn checkpoint(&mut self, check: &impl Fn()) {
        self.release();
        check();
        self.acquire();
    }
    pub fn first(&self) -> u32 {
        self.first
    }
    pub fn freespace(&self) -> u16 {
        self.head.as_ref().unwrap().freespace()
    }
    pub fn tape_move(&mut self) {
        if self.head.as_ref().unwrap().len() == 0 {
            panic!("implementation: a clear page cannot accommodate a single tuple");
        }
        let next = self.index.alloc(Opaque {
            next: u32::MAX,
            flags: 0,
            generation: self.generation,
        });
        self.current = next.id();
        let mut old = self.head.take().unwrap();
        old.get_opaque_mut().next = self.current;
        self.head = Some(self.index.commit_pair(old, next));
    }
}

impl<'a, R, T> TapeWriter<'a, R, T>
where
    R: RelationWrite + 'a,
    R::Page: Page<Opaque = Opaque>,
    T: Tuple,
{
    pub fn push(&mut self, x: T) -> (u32, u16) {
        let bytes = T::serialize(&x);
        if let Some(i) = self.head.as_mut().unwrap().alloc(&bytes) {
            (self.current, i)
        } else {
            self.tape_move();
            if let Some(i) = self.head.as_mut().unwrap().alloc(&bytes) {
                (self.current, i)
            } else {
                panic!("implementation: a free page cannot accommodate a single tuple")
            }
        }
    }
    pub fn tape_put(&mut self, x: T) -> (u32, u16) {
        let bytes = T::serialize(&x);
        if let Some(i) = self.head.as_mut().unwrap().alloc(&bytes) {
            (self.current, i)
        } else {
            panic!("implementation: a free page cannot accommodate a single tuple")
        }
    }
}

pub struct BackwardTapeWriter<'a, R, T>
where
    R: RelationWrite + 'a,
{
    head: R::WriteGuard<'a>,
    index: &'a R,
    generation: u64,
    _phantom: PhantomData<fn(T) -> T>,
}

impl<'a, R, T> BackwardTapeWriter<'a, R, T>
where
    R: RelationWrite + 'a,
    R::Page: Page<Opaque = Opaque>,
{
    pub fn create(index: &'a R, generation: u64) -> Self {
        let head = index.alloc(Opaque {
            next: u32::MAX,
            flags: 0,
            generation,
        });
        Self {
            head,
            index,
            generation,
            _phantom: PhantomData,
        }
    }
    pub fn checkpoint(self, check: &impl Fn()) -> Self {
        let Self {
            head,
            index,
            generation,
            _phantom,
        } = self;
        let id = head.id();
        drop(head);
        check();
        Self {
            head: index.write(id),
            index,
            generation,
            _phantom,
        }
    }
    pub fn into_head(self) -> R::WriteGuard<'a> {
        self.head
    }
    pub fn freespace(&self) -> u16 {
        self.head.freespace()
    }
    pub fn tape_move(&mut self) {
        if self.head.len() == 0 {
            panic!("implementation: a clear page cannot accommodate a single tuple");
        }
        self.head = self.index.alloc(Opaque {
            next: self.head.id(),
            flags: 0,
            generation: self.generation,
        });
    }
}

impl<'a, R, T> BackwardTapeWriter<'a, R, T>
where
    R: RelationWrite + 'a,
    R::Page: Page<Opaque = Opaque>,
    T: Tuple,
{
    pub fn tape_put(&mut self, x: T) -> (u32, u16) {
        let bytes = T::serialize(&x);
        if let Some(i) = self.head.alloc(&bytes) {
            (self.head.id(), i)
        } else {
            panic!("implementation: a free page cannot accommodate a single tuple")
        }
    }
}

pub struct TapeReader<T> {
    buffer: VecDeque<T>,
    next: u32,
    deserialize: fn(&[u8]) -> T,
}

impl<T> TapeReader<T> {
    pub fn new(first: u32, deserialize: fn(&[u8]) -> T) -> Self {
        Self {
            buffer: VecDeque::new(),
            next: first,
            deserialize,
        }
    }
    pub fn next<R: RelationRead>(&mut self, index: &R) -> Option<T>
    where
        R::Page: Page<Opaque = Opaque>,
    {
        while self.buffer.is_empty() && self.next != u32::MAX {
            self.next = {
                let guard = index.read(self.next);
                for j in 1..=guard.len() {
                    let bytes = guard.get(j).expect("data corruption");
                    let tuple = (self.deserialize)(bytes);
                    self.buffer.push_back(tuple);
                }
                guard.get_opaque().next
            };
        }
        self.buffer.pop_front()
    }
}

pub struct TruncatedTapeReader<T> {
    buffer: VecDeque<T>,
    next: u32,
    deserialize: fn(&[u8]) -> T,
    count: u32,
}

impl<T> TruncatedTapeReader<T> {
    pub fn new<R: RelationRead>(
        index: &R,
        first: (u32, u16),
        deserialize: fn(&[u8]) -> T,
        mut count: u32,
    ) -> Self
    where
        R::Page: Page<Opaque = Opaque>,
    {
        let mut buffer = VecDeque::new();
        let next = {
            let guard = index.read(first.0);
            for j in first.1..=guard.len() {
                if count == 0 {
                    break;
                }
                let bytes = guard.get(j).expect("data corruption");
                let tuple = deserialize(bytes);
                buffer.push_back(tuple);
                count -= 1;
            }
            guard.get_opaque().next
        };
        Self {
            buffer,
            next,
            deserialize,
            count,
        }
    }
    pub fn next<R: RelationRead>(&mut self, index: &R) -> Option<T>
    where
        R::Page: Page<Opaque = Opaque>,
    {
        while self.count != 0 && self.buffer.is_empty() && self.next != u32::MAX {
            self.next = {
                let guard = index.read(self.next);
                for j in 1..=guard.len() {
                    if self.count == 0 {
                        break;
                    }
                    let bytes = guard.get(j).expect("data corruption");
                    let tuple = (self.deserialize)(bytes);
                    self.buffer.push_back(tuple);
                    self.count -= 1;
                }
                guard.get_opaque().next
            };
        }
        self.buffer.pop_front()
    }
}
