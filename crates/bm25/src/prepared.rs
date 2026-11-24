// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU Affero General Public License v3 (AGPLv3).
// See <https://www.gnu.org/licenses/agpl-3.0.html> for details.
//
// Copyright (c) 2025-2026 TensorChord Inc.
// Copyright (c) 2026 PGSTY PTE. LTD.
// Maintained by Ruohang Feng @ PGSTY (rh@vonng.com)

use crate::bm25::{Cache, length_to_fieldnorm};
use crate::checked::{Checked, Reader, growing_page_is_newer};
use crate::growing::{Boundary, Candidates, GrowingReader, Unscored};
use crate::tuples::*;
use crate::vector::{Document, Query};
use crate::{Opaque, address_tokens};
use index::relation::{Page, RelationRead};

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Purpose {
    Score,
    Scan,
}

/// Owned query state. No pins, relation handles or memory-context pointers.
/// Counts include physical non-NULL records until compaction removes them.
/// Results separately exclude tombstones and PostgreSQL checks heap visibility.
pub struct Prepared {
    pub(crate) jump: Jump,
    pub(crate) tokens: Vec<Token>,
    pub(crate) boundary: Boundary,
    pub(crate) candidates: std::cell::OnceCell<Candidates>,
    pub(crate) bound_scale: f64,
    // Query statistics equal the sealed segment's: block bounds are exact.
    pub(crate) exact_bounds: bool,
    query: Query,
}

#[derive(Clone, Copy)]
pub(crate) struct Jump {
    pub generation: u64,
    pub ptr_vectors: u32,
    pub ptr_vectors_offset: u16,
    pub ptr_documents: u32,
    pub number_of_documents: u32,
    pub number_of_nulls: u32,
    pub sum_of_document_lengths: u64,
    pub width_1_documents: u16,
    pub width_0_documents: u16,
    pub depth_documents: u32,
    pub start_documents: u32,
    pub depth_tokens: u32,
    pub start_tokens: u32,
}

pub(crate) struct Token {
    pub number_of_documents: u32,
    pub wand_fieldnorm: u8,
    pub wand_term_frequency: u32,
    pub wptr_summaries: Option<(u32, u16)>,
    pub bm25: Cache,
    pub bound: Cache,
}

impl Prepared {
    pub fn collect<R: RelationRead>(
        index: &R,
        query: &Query,
        purpose: Purpose,
        mut check: impl FnMut(),
        reader: Reader<'_>,
    ) -> Self
    where
        R::Page: Page<Opaque = Opaque>,
    {
        let (k1, b, ptr_jump) = {
            let guard = index.read(0);
            let meta = MetaTuple::deserialize_ref(guard.get(1).expect("data corruption"));
            (meta.k1(), meta.b(), meta.ptr_jump())
        };
        let (jump, tail_hint) = {
            let guard = index.read(ptr_jump);
            let j = JumpTuple::deserialize_ref(guard.get(1).expect("data corruption"));
            (
                Jump {
                    generation: j.generation(),
                    ptr_vectors: j.ptr_vectors(),
                    ptr_vectors_offset: j.ptr_vectors_offset(),
                    ptr_documents: j.ptr_documents(),
                    number_of_documents: j.number_of_documents(),
                    number_of_nulls: j.number_of_nulls(),
                    sum_of_document_lengths: j.sum_of_document_lengths(),
                    width_1_documents: j.width_1_documents(),
                    width_0_documents: j.width_0_documents(),
                    depth_documents: j.depth_documents(),
                    start_documents: j.start_documents(),
                    depth_tokens: j.depth_tokens(),
                    start_tokens: j.start_tokens(),
                },
                j.tail_hint(),
            )
        };
        // Capture the tail before traversing documents; later appends cannot
        // extend this collection indefinitely or change its statistics.
        let cut = {
            let mut page = tail_hint;
            loop {
                let guard = index.read(page);
                let opaque = *guard.get_opaque();
                if growing_page_is_newer(opaque, jump.generation, reader) {
                    break None;
                }
                let next = opaque.next;
                let end = guard.len();
                drop(guard);
                check();
                if next == u32::MAX {
                    break Some((page, end));
                }
                page = next;
            }
        };
        let mut n = jump.number_of_documents - jump.number_of_nulls;
        let mut sum_len = jump.sum_of_document_lengths;
        let mut df = vec![0_u32; query.len()];
        let mut scan = (purpose == Purpose::Scan).then(Unscored::default);
        let boundary = GrowingReader {
            index,
            reader,
            check: &mut check,
        }
        .collect(jump, query, cut, scan.as_mut(), |len, terms| {
            n = n.checked_add(1).expect("too many documents");
            sum_len += u64::from(len);
            for &(q, _) in terms {
                df[q] += 1;
            }
        });
        let avgdl = if n == 0 {
            1.0
        } else {
            sum_len as f64 / n as f64
        };
        let sealed_n = jump.number_of_documents - jump.number_of_nulls;
        let sealed_avgdl = if sealed_n == 0 || jump.sum_of_document_lengths == 0 {
            1.0
        } else {
            jump.sum_of_document_lengths as f64 / sealed_n as f64
        };
        let sealed = Checked::new(index, jump.generation, reader);
        let mut tokens = Vec::with_capacity(query.len());
        let norms = Cache::norms(k1, b, avgdl);
        let bound_norms = Cache::norms(k1, b, sealed_avgdl);
        for (q, &key) in query.iter().enumerate() {
            let (count, fieldnorm, tf, ptr) =
                address_tokens::read(&sealed, jump.depth_tokens, jump.start_tokens, key)
                    .map(|(guard, offset)| {
                        let t = TokenTuple::deserialize_ref(
                            guard.get(offset).expect("data corruption"),
                        );
                        (
                            t.number_of_documents(),
                            t.wand_fieldnorm(),
                            t.wand_term_frequency(),
                            Some(t.wptr_summaries()),
                        )
                    })
                    .unwrap_or((0, 0, 0, None));
            let total_df = count.checked_add(df[q]).expect("too many documents");
            tokens.push(Token {
                number_of_documents: count,
                wand_fieldnorm: fieldnorm,
                wand_term_frequency: tf,
                wptr_summaries: ptr,
                bm25: Cache::new(n, total_df, k1, norms.clone()),
                bound: Cache::new(n, total_df, k1, bound_norms.clone()),
            });
            check();
        }
        let candidates = std::cell::OnceCell::new();
        if let Some(scan) = scan {
            candidates.get_or_init(|| scan.score(&tokens, &mut check));
        }
        Self {
            jump,
            tokens,
            boundary,
            candidates,
            bound_scale: (avgdl / sealed_avgdl).max(1.0),
            exact_bounds: avgdl.to_bits() == sealed_avgdl.to_bits(),
            query: query.clone(),
        }
    }

    pub fn prepare_scan<R: RelationRead>(
        &self,
        index: &R,
        reader: Reader<'_>,
        mut check: impl FnMut(),
    ) -> bool
    where
        R::Page: Page<Opaque = Opaque>,
    {
        if self.candidates.get().is_some() {
            return false;
        }
        let mut scan = Unscored::default();
        if self.boundary.count != 0 {
            GrowingReader {
                index,
                reader,
                check: &mut check,
            }
            .collect(
                self.jump,
                &self.query,
                self.boundary.cut,
                Some(&mut scan),
                |_, _| {},
            );
        }
        self.candidates
            .get_or_init(|| scan.score(&self.tokens, &mut check));
        true
    }

    /// Heap bytes held by this preparation, for cache budgeting.
    pub fn approximate_size(&self) -> usize {
        size_of::<Self>()
            + self.tokens.capacity() * size_of::<Token>()
            + self.candidates.get().map_or(0, Candidates::bytes)
            + self.query.len() * size_of::<[u8; crate::WIDTH]>()
            // Two shared normalization arrays, including Rc's two counters.
            + usize::from(!self.tokens.is_empty()) * 2 * (size_of::<[f64; 256]>() + 2 * size_of::<usize>())
    }

    pub fn evaluate(&self, document: &Document) -> f64 {
        let fieldnorm = length_to_fieldnorm(document.length());
        let mut cursor = 0;
        let mut score = 0.0;
        for (q, key) in self.query.iter().enumerate() {
            while cursor < document.len() && document.as_slice()[cursor].key < *key {
                cursor += 1;
            }
            if let Some(e) = document.as_slice().get(cursor) {
                if e.key == *key {
                    score += self.tokens[q].bm25.evaluate(fieldnorm, e.value);
                }
            }
        }
        score
    }
}
