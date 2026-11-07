// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU Affero General Public License v3 (AGPLv3).
// See <https://www.gnu.org/licenses/agpl-3.0.html> for details.
//
// Copyright (c) 2025-2026 TensorChord Inc.
// Copyright (c) 2026 PGSTY PTE. LTD.
// Maintained by Ruohang Feng @ PGSTY (rh@vonng.com)

use crate::Opaque;
use crate::bm25::length_to_fieldnorm;
use crate::tape::TapeWriter;
use crate::tuples::{JumpTuple, MetaTuple, VectorTuple, WithReader, WithWriter};
use crate::vector::Document;
use index::relation::{Opaque as _, Page, RelationRead, RelationWrite};
use index::tuples::Bool;

pub fn insert<R: RelationRead + RelationWrite>(
    index: &R,
    document: &Document,
    payload: [u16; 3],
    check: impl Fn(),
) where
    R::Page: Page<Opaque = Opaque>,
{
    let document_length = document.length();
    let fieldnorm = length_to_fieldnorm(document_length);

    let meta_guard = index.read(0);
    let meta_bytes = meta_guard.get(1).expect("data corruption");
    let meta_tuple = MetaTuple::deserialize_ref(meta_bytes);
    let ptr_jump = meta_tuple.ptr_jump();
    drop(meta_guard);

    let (jump_guard, head, generation, first) = 'retry: loop {
        check();
        let (generation, first) = {
            let guard = index.read(ptr_jump);
            let jump = JumpTuple::deserialize_ref(guard.get(1).expect("data corruption"));
            (jump.generation(), jump.tail_hint())
        };
        let mut current = first;
        loop {
            // Wait for the current append without holding jump. In particular,
            // a queue of inserters must not keep publication's shared lock held.
            let read = index.read(current);
            let opaque = *read.get_opaque();
            drop(read);
            if opaque.is_deleted() || opaque.generation > generation {
                continue 'retry;
            }
            if opaque.next != u32::MAX {
                current = opaque.next;
                check();
                continue;
            }
            let jump_guard = index.read(ptr_jump);
            let jump = JumpTuple::deserialize_ref(jump_guard.get(1).expect("data corruption"));
            if jump.generation() != generation {
                continue 'retry;
            }
            // Preserve jump -> page order, but never WAIT for a content lock
            // while holding jump. A failed attempt drops jump before retrying.
            let Some(head) = index.try_write(current) else {
                continue 'retry;
            };
            if head.get_opaque().next != u32::MAX {
                index.discard(head);
                continue 'retry;
            }
            break 'retry (jump_guard, head, generation, first);
        }
    };

    // Retain jump for one append so publication cannot change its generation.
    // A large document can still extend the relation while holding this lock.
    let mut tape = TapeWriter::from_guard(index, head, generation);
    tape.push(VectorTuple::_2 {
        fieldnorm,
        is_null: Bool::from(document.is_null()),
    });
    let mut remain = document.as_slice();
    loop {
        let freespace = tape.freespace();
        if VectorTuple::estimate_size_0(remain.len()) <= freespace as usize {
            tape.tape_put(VectorTuple::_0 {
                deleted: Bool::FALSE,
                payload,
                elements: remain.to_vec(),
            });
            break;
        }
        if let Some(w) = VectorTuple::fit_1(freespace) {
            let (left, right) = remain.split_at(std::cmp::min(w, remain.len()));
            tape.tape_put(VectorTuple::_1 {
                elements: left.to_vec(),
            });
            remain = right;
        } else {
            tape.tape_move();
        }
    }
    let tail = tape.finish();
    drop(jump_guard);
    if tail != first {
        let mut guard = index.write(ptr_jump);
        let mut jump = JumpTuple::deserialize_mut(guard.get_mut(1).expect("data corruption"));
        if *jump.generation() == generation && *jump.tail_hint() == first {
            *jump.tail_hint() = tail;
        }
    }
}
