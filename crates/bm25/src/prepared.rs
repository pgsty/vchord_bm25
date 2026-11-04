// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU Affero General Public License v3 (AGPLv3).
// See <https://www.gnu.org/licenses/agpl-3.0.html> for details.
//
// Copyright (c) 2025-2026 TensorChord Inc.
// Copyright (c) 2026 PGSTY PTE. LTD.
// Maintained by Ruohang Feng @ PGSTY (rh@vonng.com)

use crate::bm25::{Cache, length_to_fieldnorm};
use crate::checked::{Checked, Reader, growing_page_is_newer};
use crate::tuples::*;
use crate::vector::{Document, Element, Query};
use crate::{Opaque, address_tokens};
use index::relation::{Page, RelationRead};

/// Owned query state. No pins, relation handles or memory-context pointers.
/// Counts include physical non-NULL records until compaction removes them.
/// Results separately exclude tombstones and PostgreSQL checks heap visibility.
pub struct Prepared {
    pub(crate) jump: Jump,
    pub(crate) tokens: Vec<Token>,
    pub(crate) growing: Vec<Growing>,
    pub(crate) bound_scale: f64,
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

pub(crate) struct Growing {
    pub payload: [u16; 3],
    pub score: f64, // -infinity denotes SQL NULL, ordered after zero.
}

impl Prepared {
    pub fn collect<R: RelationRead>(
        index: &R,
        query: &Query,
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
        let mut documents = Vec::new();
        let mut state = None;
        let mut page = jump.ptr_vectors;
        let mut start = jump.ptr_vectors_offset;
        while page != u32::MAX {
            let guard = index.read(page);
            let opaque = *guard.get_opaque();
            if growing_page_is_newer(opaque, jump.generation, reader) {
                break;
            }
            let end = if let Some((last, end)) = cut {
                if page == last { end } else { guard.len() }
            } else {
                guard.len()
            };
            for i in start..=end {
                match VectorTuple::deserialize_ref(guard.get(i).expect("data corruption")) {
                    VectorTupleReader::_2(t) => {
                        state = Some((
                            t.fieldnorm(),
                            t.is_null(),
                            0_u32,
                            Vec::<(usize, u32)>::new(),
                        ))
                    }
                    VectorTupleReader::_1(t) => {
                        let (_, _, len, terms) = state.as_mut().expect("incomplete vector");
                        collect_elements(t.elements(), query, len, terms);
                    }
                    VectorTupleReader::_0(t) => {
                        let (fieldnorm, is_null, mut len, mut terms) =
                            state.take().expect("incomplete vector");
                        collect_elements(t.elements(), query, &mut len, &mut terms);
                        if !is_null {
                            n = n.checked_add(1).expect("too many documents");
                            sum_len += u64::from(len);
                            for &(q, _) in &terms {
                                df[q] += 1;
                            }
                        }
                        if !bool::from(t.deleted()) {
                            documents.push((t.payload(), fieldnorm, is_null, terms));
                        }
                    }
                }
            }
            let next = opaque.next;
            drop(guard);
            check();
            if cut.is_some_and(|(last, _)| page == last) {
                break;
            }
            page = next;
            start = 1;
        }
        // A crash can leave an unfinished, uncommitted last document. It has
        // no payload/completion record and never contributes to this query.
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
                bm25: Cache::new(n, total_df, k1, b, avgdl),
                bound: Cache::new(n, total_df, k1, b, sealed_avgdl),
            });
            check();
        }
        let growing = documents
            .into_iter()
            .enumerate()
            .map(|(i, (payload, fieldnorm, is_null, terms))| {
                if i % 1024 == 0 {
                    check();
                }
                let score = if is_null {
                    f64::NEG_INFINITY
                } else {
                    terms
                        .into_iter()
                        .map(|(q, tf)| tokens[q].bm25.evaluate(fieldnorm, tf))
                        .sum()
                };
                Growing { payload, score }
            })
            .collect();
        Self {
            jump,
            tokens,
            growing,
            bound_scale: (avgdl / sealed_avgdl).max(1.0),
            query: query.clone(),
        }
    }

    /// Heap bytes held by this preparation, for cache budgeting.
    pub fn approximate_size(&self) -> usize {
        size_of::<Self>()
            + self.tokens.capacity() * size_of::<Token>()
            + self.growing.capacity() * size_of::<Growing>()
            + self.query.len() * size_of::<[u8; crate::WIDTH]>()
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

fn collect_elements(
    elements: &[Element],
    query: &Query,
    len: &mut u32,
    terms: &mut Vec<(usize, u32)>,
) {
    for e in elements {
        *len = len.saturating_add(e.value);
        if let Ok(q) = query.as_slice().binary_search(&e.key) {
            terms.push((q, e.value));
        }
    }
}
