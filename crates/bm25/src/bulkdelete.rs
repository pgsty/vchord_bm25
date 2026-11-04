// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU Affero General Public License v3 (AGPLv3).
// See <https://www.gnu.org/licenses/agpl-3.0.html> for details.
//
// Copyright (c) 2025-2026 TensorChord Inc.
// Copyright (c) 2026 PGSTY PTE. LTD.
// Maintained by Ruohang Feng @ PGSTY (rh@vonng.com)

use crate::Opaque;
use crate::tuples::*;
use index::relation::{Page, RelationRead, RelationWrite};
use index::tuples::Bool;

pub fn bulkdelete<R: RelationRead + RelationWrite>(
    index: &R,
    check: impl Fn(),
    callback: impl Fn([u16; 3]) -> bool,
) -> u64
where
    R::Page: Page<Opaque = Opaque>,
{
    let ptr_jump = {
        let g = index.read(0);
        MetaTuple::deserialize_ref(g.get(1).expect("data corruption")).ptr_jump()
    };
    let (vectors, offset, documents) = {
        let g = index.read(ptr_jump);
        let j = JumpTuple::deserialize_ref(g.get(1).expect("data corruption"));
        (j.ptr_vectors(), j.ptr_vectors_offset(), j.ptr_documents())
    };
    let mut removed = 0_u64;
    for (mut page, mut start, growing) in [(vectors, offset, true), (documents, 1, false)] {
        while page != u32::MAX {
            let (next, candidates) = {
                let g = index.read(page);
                let mut candidates = Vec::new();
                for i in start..=g.len() {
                    let bytes = g.get(i).expect("data corruption");
                    let entry = if growing {
                        match VectorTuple::deserialize_ref(bytes) {
                            VectorTupleReader::_0(t) => Some((t.deleted(), t.payload())),
                            _ => None,
                        }
                    } else {
                        let t = DocumentTuple::deserialize_ref(bytes);
                        Some((t.deleted(), t.payload()))
                    };
                    if let Some((deleted, payload)) = entry {
                        if !bool::from(deleted) {
                            candidates.push((i, payload));
                        }
                    }
                }
                (g.get_opaque().next, candidates)
            };
            let offsets = candidates
                .into_iter()
                .filter_map(|(i, tid)| callback(tid).then_some(i))
                .collect::<Vec<_>>();
            if !offsets.is_empty() {
                let mut g = index.write(page);
                for i in offsets {
                    let bytes = g.get_mut(i).expect("data corruption");
                    if growing {
                        let VectorTupleWriter::_0(mut t) = VectorTuple::deserialize_mut(bytes)
                        else {
                            unreachable!()
                        };
                        *t.deleted() = Bool::TRUE;
                    } else {
                        *DocumentTuple::deserialize_mut(bytes).deleted() = Bool::TRUE;
                    }
                    removed += 1;
                }
            }
            check();
            page = next;
            start = 1;
        }
    }
    removed
}
