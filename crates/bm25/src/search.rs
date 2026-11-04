// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU Affero General Public License v3 (AGPLv3).
// See <https://www.gnu.org/licenses/agpl-3.0.html> for details.
//
// Copyright (c) 2025-2026 TensorChord Inc.
// Copyright (c) 2026 PGSTY PTE. LTD.
// Maintained by Ruohang Feng @ PGSTY (rh@vonng.com)

use crate::bm25::Cache;
use crate::checked::{Checked, Reader};
use crate::prepared::Prepared;
use crate::tape::TruncatedTapeReader;
use crate::tuples::*;
use crate::{Opaque, address_documents, compression};
use index::relation::{Page, RelationRead};
use score::Score;
use std::cmp::{Ordering, Reverse};
use std::collections::{BinaryHeap, HashSet};
use std::iter::chain;
use std::num::NonZero;

pub fn search<R: RelationRead>(
    index: &R,
    k: NonZero<usize>,
    prepared: &Prepared,
    after: Option<(f64, [u16; 3])>,
    mut filter: impl FnMut([u16; 3]) -> bool,
    mut check: impl FnMut(),
    reader: Reader<'_>,
) -> Vec<(Reverse<Score>, [u16; 3])>
where
    R::Page: Page<Opaque = Opaque>,
{
    let after = after.map(|(score, tid)| (Reverse(Score::from_f64(score)), tid));
    let remaining = |score: f64, tid: [u16; 3]| {
        after.is_none_or(|a| (Reverse(Score::from_f64(score)), tid) > a)
    };
    let jump = prepared.jump;
    let sealed = Checked::new(index, jump.generation, reader);
    let index = &sealed;
    let mut results = Results::<[u16; 3]>::new(k, 0.0);
    let mut matched = HashSet::new();
    for growing in &prepared.growing {
        check();
        if growing.score > 0.0
            && remaining(growing.score, growing.payload)
            && results.threshold() <= growing.score
            && filter(growing.payload)
        {
            results.push(growing.score, growing.payload);
        }
    }

    let mut cursors = Vec::new();
    for (ordinal, token) in prepared.tokens.iter().enumerate() {
        if let Some(wptr_summaries) = token.wptr_summaries {
            cursors.push(Box::new(Cursor::new(
                index,
                token.number_of_documents,
                token.wand_fieldnorm,
                token.wand_term_frequency,
                wptr_summaries,
                token.bm25.clone(),
                token.bound.clone(),
                prepared.bound_scale,
                ordinal,
            )));
        }
        check();
    }

    let mut tail = Vec::<Box<Cursor>>::new();
    let mut head = BinaryHeap::from(cursors);
    'main: loop {
        check();
        let lead = 'lead: {
            let mut sum = 0.0f64;
            for cursor in tail.iter() {
                sum = (sum + cursor.token_upper_bound()).next_up();
            }
            while let Some(cursor) = head.pop() {
                if cursor.document_id() == u32::MAX {
                    break 'main;
                }
                if results.threshold() <= (sum + cursor.token_upper_bound()).next_up() {
                    break 'lead cursor;
                } else {
                    sum = (sum + cursor.token_upper_bound()).next_up();
                    tail.push(cursor);
                }
            }
            break 'main;
        };
        let document_id = lead.document_id();
        let mut lead = vec![lead];
        while let Some(cursor) =
            binary_heap_pop_if(&mut head, |cursor| document_id == cursor.document_id())
        {
            lead.push(cursor);
        }
        {
            let mut failures = tail.extract_if(.., |cursor| {
                cursor.seek_block(index, document_id, &mut check);
                document_id < cursor.document_id()
            });
            if let Some(failure) = failures.next() {
                for cursor in lead {
                    head.push(cursor);
                }
                head.push(failure);
                for failure in failures {
                    head.push(failure);
                }
                continue 'main;
            }
        }
        let sum_of_block_upper_bounds = {
            let mut result = 0.0;
            for cursor in tail.iter() {
                result = (result + cursor.block_upper_bound()).next_up();
            }
            for cursor in lead.iter() {
                result = (result + cursor.block_upper_bound()).next_up();
            }
            result
        };
        if results.threshold() <= sum_of_block_upper_bounds {
            {
                let mut failures = tail.extract_if(.., |cursor| {
                    cursor.seek(index, document_id, &mut check);
                    document_id < cursor.document_id()
                });
                if let Some(failure) = failures.next() {
                    for cursor in lead {
                        head.push(cursor);
                    }
                    head.push(failure);
                    continue 'main;
                }
            };
            let (fieldnorm, payload, deleted) = {
                let (document_guard, document_i) = address_documents::read(
                    index,
                    jump.width_1_documents,
                    jump.width_0_documents,
                    jump.depth_documents,
                    jump.start_documents,
                    document_id,
                )
                .expect("data corruption");
                let document_bytes = document_guard.get(document_i).expect("data corruption");
                let document_tuple = DocumentTuple::deserialize_ref(document_bytes);
                (
                    document_tuple.fieldnorm(),
                    document_tuple.payload(),
                    bool::from(document_tuple.deleted()),
                )
            };
            check();
            matched.insert(document_id);
            if !deleted {
                let mut result = 0.0;
                let mut ordered = chain(tail.iter_mut(), lead.iter_mut()).collect::<Vec<_>>();
                ordered.sort_unstable_by_key(|c| c.ordinal);
                for cursor in ordered {
                    let term_frequency = cursor.get(index);
                    result += cursor.bm25().evaluate(fieldnorm, term_frequency);
                }
                if remaining(result, payload) && filter(payload) {
                    results.push(result, payload);
                }
                check();
            }
            for mut cursor in chain(tail, lead) {
                cursor.seek(index, 1 + document_id, &mut check);
                head.push(cursor);
            }
            tail = Vec::new();
        } else {
            let min_of_block_max_document_ids = {
                let mut result = u32::MAX;
                for cursor in lead.iter() {
                    result = result.min(cursor.block_max_document_id());
                }
                for cursor in tail.iter() {
                    result = result.min(cursor.block_max_document_id());
                }
                result
            };
            let seek_document_id = std::cmp::min(
                1 + min_of_block_max_document_ids,
                head.peek()
                    .map(|cursor| cursor.document_id())
                    .unwrap_or(u32::MAX),
            );
            let mut cursor = {
                let array = [&mut lead, &mut tail];
                let mut max = f64::NEG_INFINITY;
                let mut argmax = (0, 0);
                for i in 0..array.len() {
                    for j in 0..array[i].len() {
                        if array[i][j].token_upper_bound() > max {
                            max = array[i][j].token_upper_bound();
                            argmax = (i, j);
                        }
                    }
                }
                array[argmax.0].remove(argmax.1)
            };
            cursor.seek(index, seek_document_id, &mut check);
            head.push(cursor);
            for cursor in lead.into_iter() {
                head.push(cursor);
            }
        }
    }
    if results.internal.len() < k.get() {
        // No positive result was pruned when the heap never filled. Complete
        // SQL ORDER BY with zero scores and NULLs, including empty queries.
        let mut page = jump.ptr_documents;
        let mut id = 0_u32;
        while page != u32::MAX {
            let (next, docs) = {
                let guard = index.read(page);
                let docs = (1..=guard.len())
                    .map(|i| {
                        let t =
                            DocumentTuple::deserialize_ref(guard.get(i).expect("data corruption"));
                        (t.payload(), bool::from(t.deleted()), t.is_null())
                    })
                    .collect::<Vec<_>>();
                (guard.get_opaque().next, docs)
            };
            for (payload, deleted, is_null) in docs {
                let score = if is_null { f64::NEG_INFINITY } else { 0.0 };
                if !deleted
                    && !matched.contains(&id)
                    && remaining(score, payload)
                    && filter(payload)
                {
                    results.push(score, payload);
                }
                id += 1;
                check();
            }
            page = next;
        }
        for growing in &prepared.growing {
            if growing.score <= 0.0
                && remaining(growing.score, growing.payload)
                && filter(growing.payload)
            {
                results.push(growing.score, growing.payload);
            }
            check();
        }
    }
    results.into_sorted_vec()
}

struct Results<T: Ord> {
    limit: NonZero<usize>,
    threshold: Score,
    internal: BinaryHeap<(Reverse<Score>, T)>,
}

impl<T: Ord> Results<T> {
    fn new(limit: NonZero<usize>, threshold: f64) -> Self {
        Self {
            limit,
            threshold: Score::from_f64(threshold),
            internal: BinaryHeap::new(),
        }
    }
    fn threshold(&self) -> f64 {
        self.threshold.to_f64()
    }
    fn push(&mut self, key: f64, value: T) {
        self.internal.push((Reverse(Score::from_f64(key)), value));
        if self.internal.len() > self.limit.get() {
            self.internal.pop();
        }
        if self.internal.len() == self.limit.get() {
            self.threshold = self.threshold.max(self.internal.peek().unwrap().0.0);
        }
    }
    fn into_sorted_vec(self) -> Vec<(Reverse<Score>, T)> {
        self.internal.into_sorted_vec()
    }
}

struct Cursor {
    bm25: Cache,
    bound: Cache,
    bound_scale: f64,
    ordinal: usize,
    token_upper_bound: f64,

    document_id: u32,
    position_in_block: u8,

    incoming: TruncatedTapeReader<Summary>,

    summary: Summary,
    block_upper_bound: f64,
    filled: bool,
    block: Block,
}

impl PartialEq for Cursor {
    fn eq(&self, other: &Self) -> bool {
        PartialEq::eq(&other.document_id, &self.document_id)
    }
}

impl Eq for Cursor {}

impl PartialOrd for Cursor {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(Ord::cmp(&other.document_id, &self.document_id))
    }
}

impl Ord for Cursor {
    fn cmp(&self, other: &Self) -> Ordering {
        Ord::cmp(&other.document_id, &self.document_id)
    }
}

impl Cursor {
    fn new<R: RelationRead>(
        index: &R,
        token_number_of_documents: u32,
        token_wand_fieldnorm: u8,
        token_wand_term_frequency: u32,
        wptr_summaries: (u32, u16),
        bm25: Cache,
        bound: Cache,
        bound_scale: f64,
        ordinal: usize,
    ) -> Self
    where
        R::Page: Page<Opaque = Opaque>,
    {
        let token_upper_bound = upper_bound(
            &bound,
            bound_scale,
            token_wand_fieldnorm,
            token_wand_term_frequency,
        );
        let mut incoming = TruncatedTapeReader::new(
            index,
            wptr_summaries,
            |bytes| {
                let summary_tuple = SummaryTuple::deserialize_ref(bytes);
                Summary {
                    min_document_id: summary_tuple.min_document_id(),
                    max_document_id: summary_tuple.max_document_id(),
                    number_of_documents: summary_tuple.number_of_documents(),
                    wand_fieldnorm: summary_tuple.wand_fieldnorm(),
                    wand_term_frequency: summary_tuple.wand_term_frequency(),
                    wptr_block: summary_tuple.wptr_block().into_inner(),
                }
            },
            token_number_of_documents.div_ceil(128),
        );
        let summary = next_summary(&mut incoming, index);
        let block_upper_bound = upper_bound(
            &bound,
            bound_scale,
            summary.wand_fieldnorm,
            summary.wand_term_frequency,
        );
        Cursor {
            bm25,
            bound,
            bound_scale,
            ordinal,
            token_upper_bound,
            document_id: summary.min_document_id,
            position_in_block: 0,
            summary,
            block_upper_bound,
            filled: false,
            block: Block {
                document_ids: compression::Decompressed::new(),
                term_frequencies: compression::Decompressed::new(),
            },
            incoming,
        }
    }
    fn bm25(&self) -> &Cache {
        &self.bm25
    }
    fn token_upper_bound(&self) -> f64 {
        self.token_upper_bound
    }
    fn document_id(&self) -> u32 {
        self.document_id
    }
    fn block_max_document_id(&self) -> u32 {
        self.summary.max_document_id
    }
    fn block_upper_bound(&self) -> f64 {
        self.block_upper_bound
    }
    fn seek_block<R: RelationRead>(&mut self, index: &R, document_id: u32, check: &mut impl FnMut())
    where
        R::Page: Page<Opaque = Opaque>,
    {
        assert!(document_id < u32::MAX);
        debug_assert!(document_id >= self.document_id);
        if document_id <= self.summary.max_document_id {
            return;
        }
        while self.summary.max_document_id < document_id {
            self.summary = next_summary(&mut self.incoming, index);
            check();
        }
        self.document_id = self.summary.min_document_id;
        self.position_in_block = 0;
        self.block_upper_bound = upper_bound(
            &self.bound,
            self.bound_scale,
            self.summary.wand_fieldnorm,
            self.summary.wand_term_frequency,
        );
        self.filled = false;
    }
    fn seek<R: RelationRead>(&mut self, index: &R, document_id: u32, check: &mut impl FnMut())
    where
        R::Page: Page<Opaque = Opaque>,
    {
        self.seek_block(index, document_id, check);
        if document_id <= self.document_id {
            return;
        }
        if document_id == self.summary.max_document_id {
            self.document_id = self.summary.max_document_id;
            self.position_in_block = self.summary.number_of_documents - 1;
            return;
        }
        if !self.filled {
            fill_block(
                &mut self.block,
                index,
                self.summary.min_document_id,
                self.summary.wptr_block,
            );
            self.filled = true;
            check();
        }
        (self.document_id, self.position_in_block) = {
            let document_ids = self.block.document_ids.as_slice();
            let i = if document_id == self.document_id + 1 {
                self.position_in_block + 1
            } else {
                let start = self.position_in_block + 1;
                let (Ok(delta) | Err(delta)) =
                    document_ids[start as usize..].binary_search(&document_id);
                start + delta as u8
            };
            (document_ids[i as usize], i)
        };
    }
    fn get<R: RelationRead>(&mut self, index: &R) -> u32
    where
        R::Page: Page<Opaque = Opaque>,
    {
        if !self.filled {
            fill_block(
                &mut self.block,
                index,
                self.summary.min_document_id,
                self.summary.wptr_block,
            );
            self.filled = true;
        }
        self.block.term_frequencies.as_slice()[self.position_in_block as usize]
    }
}

fn next_summary<R: RelationRead>(incoming: &mut TruncatedTapeReader<Summary>, index: &R) -> Summary
where
    R::Page: Page<Opaque = Opaque>,
{
    incoming.next(index).unwrap_or(Summary {
        min_document_id: u32::MAX,
        max_document_id: u32::MAX,
        number_of_documents: 1,
        wand_fieldnorm: u8::MAX,
        wand_term_frequency: 0_u32,
        wptr_block: (u32::MAX, 0),
    })
}

fn fill_block<R: RelationRead>(
    block: &mut Block,
    index: &R,
    min_document_id: u32,
    ptr_block: (u32, u16),
) {
    let block_guard = index.read(ptr_block.0);
    let block_bytes = block_guard.get(ptr_block.1).expect("data corruption");
    let block_tuple = BlockTuple::deserialize_ref(block_bytes);
    compression::decompress_document_ids(
        min_document_id,
        block_tuple.metadata_document_ids(),
        block_tuple.compressed_document_ids(),
        &mut block.document_ids,
    );
    compression::decompress_term_frequencies(
        block_tuple.metadata_term_frequencies(),
        block_tuple.compressed_term_frequencies(),
        &mut block.term_frequencies,
    );
}

struct Summary {
    min_document_id: u32,
    max_document_id: u32,
    number_of_documents: u8,
    wand_fieldnorm: u8,
    wand_term_frequency: u32,
    wptr_block: (u32, u16),
}

struct Block {
    document_ids: compression::Decompressed,
    term_frequencies: compression::Decompressed,
}

// Emulate unstable library feature `binary_heap_pop_if`.
// See https://github.com/rust-lang/rust/issues/151828.

fn binary_heap_pop_if<T: Ord>(
    this: &mut BinaryHeap<T>,
    predicate: impl FnOnce(&T) -> bool,
) -> Option<T> {
    let top = this.peek()?;
    if predicate(top) { this.pop() } else { None }
}

// Round outward: the real-valued scaling inequality must remain an upper
// bound after floating-point evaluation and summation. Zero TF is a sentinel.
fn upper_bound(cache: &Cache, scale: f64, fieldnorm: u8, tf: u32) -> f64 {
    if tf == 0 {
        0.0
    } else {
        (cache.evaluate(fieldnorm, tf) * scale * (1.0 + 16.0 * f64::EPSILON)).next_up()
    }
}

#[cfg(test)]
mod tests {
    use super::upper_bound;
    use crate::bm25::{Cache, Wand};

    #[test]
    fn scaled_wand_bound_covers_every_document_after_length_drift() {
        let mut random = 20261103_u64;
        let mut next = || {
            random = random.wrapping_mul(6364136223846793005).wrapping_add(1);
            (random >> 32) as u32
        };
        for k1 in [1.2, 2.0] {
            for b in [0.0, 0.75, 1.0] {
                for drift in [0.001, 0.1, 1.0, 10.0, 1000.0] {
                    for _ in 0..100 {
                        let avg_build = f64::from(next() % 10000 + 1);
                        let avg_query = avg_build * drift;
                        let mut wand = Wand::new();
                        let documents = (0..128)
                            .map(|_| ((next() % 128) as u8, next() % 256 + 1))
                            .collect::<Vec<_>>();
                        for &(norm, tf) in &documents {
                            wand.push(norm, tf, k1, b, avg_build);
                        }
                        let build = Cache::new(10000, 3000, k1, b, avg_build);
                        let query = Cache::new(10000, 3000, k1, b, avg_query);
                        let bound = upper_bound(
                            &build,
                            drift.max(1.0),
                            wand.fieldnorm(),
                            wand.term_frequency(),
                        );
                        for (norm, tf) in documents {
                            assert!(
                                bound >= query.evaluate(norm, tf),
                                "unsafe bound: k1={k1} b={b} drift={drift}"
                            );
                        }
                    }
                }
            }
        }
    }
}
