// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU Affero General Public License v3 (AGPLv3).
// See <https://www.gnu.org/licenses/agpl-3.0.html> for details.
//
// Copyright (c) 2025-2026 TensorChord Inc.
// Copyright (c) 2026 PGSTY PTE. LTD.
// Maintained by Ruohang Feng @ PGSTY (rh@vonng.com)

use crate::Opaque;
use crate::segment::{Mapping, Record, Segment};
use crate::tape::TapeWriter;
use crate::tuples::*;
use crate::types::Bm25IndexOptions;
use index::relation::{Page, RelationWrite};

pub fn build<R: RelationWrite, D, M>(
    bm25_options: Bm25IndexOptions,
    index: &R,
    seed: [u8; 32],
    segment: Segment<D, M>,
) where
    R::Page: Page<Opaque = Opaque>,
    D: IntoIterator<Item = Record>,
    M: IntoIterator<Item = Mapping>,
{
    let k1 = bm25_options.k1;
    let b = bm25_options.b;

    let mut meta = TapeWriter::<_, MetaTuple>::create(index);
    assert_eq!(meta.first(), 0);

    let flushed = crate::flush::flush(bm25_options.k1, bm25_options.b, index, segment);

    let tape_vectors = TapeWriter::<_, VectorTuple>::create(index);

    let mut tape_jump = TapeWriter::<_, JumpTuple>::create(index);
    let ptr_jump = tape_jump.push(JumpTuple {
        ptr_vectors: { tape_vectors }.first(),
        number_of_documents: flushed.number_of_documents,
        sum_of_document_lengths: flushed.sum_of_document_lengths,
        width_1_documents: flushed.width_1_documents,
        width_0_documents: flushed.width_0_documents,
        depth_documents: flushed.depth_documents,
        start_documents: flushed.start_documents,
        free_documents: flushed.free_documents,
        depth_tokens: flushed.depth_tokens,
        start_tokens: flushed.start_tokens,
        free_tokens: flushed.free_tokens,
        ptr_documents: flushed.ptr_documents,
        ptr_tokens: flushed.ptr_tokens,
        ptr_summaries: flushed.ptr_summaries,
        ptr_blocks: flushed.ptr_blocks,
    });
    assert_eq!(ptr_jump.1, 1);

    let tape_lock = TapeWriter::<_, ()>::create(index);

    meta.push(MetaTuple {
        k1,
        b,
        ptr_lock: { tape_lock }.first(),
        ptr_jump: ptr_jump.0,
        seed,
    });
}
