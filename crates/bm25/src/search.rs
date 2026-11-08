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
use std::collections::BinaryHeap;
use std::num::NonZero;

/// Returns the best `k` documents after `after`, best first, as
/// `(score, key, payload)`.
///
/// Results are totally ordered by descending score, then ascending internal
/// key: growing documents by their position in `prepared`, then sealed
/// documents by document id. Keys are stable for one preparation, so `after`
/// resumes a scan exactly. WAND visits sealed documents in key order and every
/// key already in the heap is smaller, so a document that only ties the k-th
/// score can never enter it: ties are pruned with strict comparisons.
pub fn search<R: RelationRead>(
    index: &R,
    k: NonZero<usize>,
    prepared: &Prepared,
    after: Option<(f64, u64)>,
    mut filter: impl FnMut([u16; 3]) -> bool,
    mut check: impl FnMut(),
    reader: Reader<'_>,
) -> Vec<(f64, u64, [u16; 3])>
where
    R::Page: Page<Opaque = Opaque>,
{
    let after = after.map(|(score, key)| (Reverse(Score::from_f64(score)), key));
    let remaining =
        |score: f64, key: u64| after.is_none_or(|a| (Reverse(Score::from_f64(score)), key) > a);
    let jump = prepared.jump;
    let sealed = Checked::new(index, jump.generation, reader);
    let index = &sealed;
    let base = prepared.growing.len() as u64;
    let mut results = Results::new(k);
    // Sealed documents evaluated while the heap was not full, in id order.
    let mut matched = Vec::<u32>::new();
    for (i, growing) in prepared.growing.iter().enumerate() {
        check();
        let key = i as u64;
        if growing.score > 0.0
            && remaining(growing.score, key)
            && results.threshold() <= growing.score
            && filter(growing.payload)
        {
            results.push(growing.score, key, growing.payload);
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
                prepared.exact_bounds,
                ordinal,
            )));
        }
        check();
    }
    let exact = prepared.exact_bounds;

    // Buffers reused for every document: summing in ordinal (query) order,
    // as scalar scoring does, must not allocate per document.
    let mut terms = Vec::<(usize, f64)>::with_capacity(cursors.len());
    let mut tail = Vec::<Box<Cursor>>::with_capacity(cursors.len());
    let mut lead = Vec::<Box<Cursor>>::with_capacity(cursors.len());
    let mut head = BinaryHeap::from(cursors);
    'main: loop {
        check();
        let pivot = 'lead: {
            let mut sum = 0.0f64;
            for cursor in tail.iter() {
                sum = add_up(sum, cursor.token_upper_bound());
            }
            while let Some(cursor) = head.pop() {
                if cursor.document_id() == u32::MAX {
                    break 'main;
                }
                if results.threshold() < add_up(sum, cursor.token_upper_bound()) {
                    break 'lead cursor;
                } else {
                    sum = add_up(sum, cursor.token_upper_bound());
                    tail.push(cursor);
                }
            }
            break 'main;
        };
        let document_id = pivot.document_id();
        lead.clear();
        lead.push(pivot);
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
                head.push(failure);
                for failure in failures {
                    head.push(failure);
                }
                head.extend(lead.drain(..));
                continue 'main;
            }
        }
        let sum_of_block_upper_bounds = if exact {
            terms.clear();
            terms.extend(
                tail.iter()
                    .chain(lead.iter())
                    .map(|c| (c.ordinal, c.block_upper_bound())),
            );
            terms.sort_unstable_by_key(|&(ordinal, _)| ordinal);
            terms.iter().fold(0.0, |sum, &(_, bound)| sum + bound)
        } else {
            let mut result = 0.0;
            for cursor in tail.iter().chain(lead.iter()) {
                result = add_up(result, cursor.block_upper_bound());
            }
            result
        };
        let mut evaluate = results.threshold() < sum_of_block_upper_bounds;
        if evaluate && !exact {
            // With length drift, the summaries only give scaled bounds. Each
            // block bound is at least the score of its stored pair; when the
            // threshold is not below those scores' sum, read the blocks for
            // exact bounds before paying for their documents.
            terms.clear();
            terms.extend(
                tail.iter()
                    .chain(lead.iter())
                    .map(|c| (c.ordinal, c.block_pair_score())),
            );
            terms.sort_unstable_by_key(|&(ordinal, _)| ordinal);
            let floor = terms.iter().fold(0.0, |sum, &(_, score)| sum + score);
            if floor <= results.threshold() {
                terms.clear();
                for cursor in tail.iter_mut().chain(lead.iter_mut()) {
                    terms.push((cursor.ordinal, cursor.tight_block_upper_bound(index)));
                }
                terms.sort_unstable_by_key(|&(ordinal, _)| ordinal);
                let tight = terms.iter().fold(0.0, |sum, &(_, bound)| sum + bound);
                evaluate = results.threshold() < tight;
            }
        }
        if evaluate {
            {
                let mut failures = tail.extract_if(.., |cursor| {
                    cursor.seek(index, document_id, &mut check);
                    document_id < cursor.document_id()
                });
                if let Some(failure) = failures.next() {
                    head.push(failure);
                    head.extend(lead.drain(..));
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
            // Only a heap that never fills reaches the zero-score documents.
            if !results.full() {
                matched.push(document_id);
            }
            tail.append(&mut lead);
            if !deleted {
                terms.clear();
                for cursor in tail.iter_mut() {
                    let term_frequency = cursor.get(index);
                    terms.push((
                        cursor.ordinal,
                        cursor.bm25().evaluate(fieldnorm, term_frequency),
                    ));
                }
                terms.sort_unstable_by_key(|&(ordinal, _)| ordinal);
                let result = terms.iter().fold(0.0, |sum, &(_, score)| sum + score);
                let key = base + u64::from(document_id);
                if remaining(result, key) && filter(payload) {
                    results.push(result, key, payload);
                }
                check();
            }
            for mut cursor in tail.drain(..) {
                cursor.seek(index, 1 + document_id, &mut check);
                head.push(cursor);
            }
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
            head.extend(lead.drain(..));
        }
    }
    if !results.full() {
        // No positive result was pruned when the heap never filled. Complete
        // SQL ORDER BY with zero scores and NULLs, including empty queries.
        let mut page = jump.ptr_documents;
        let mut id = 0_u32;
        let mut skip = 0;
        let mut docs = Vec::new();
        while page != u32::MAX {
            let next = {
                let guard = index.read(page);
                docs.clear();
                docs.extend((1..=guard.len()).map(|i| {
                    let t = DocumentTuple::deserialize_ref(guard.get(i).expect("data corruption"));
                    (t.payload(), bool::from(t.deleted()), t.is_null())
                }));
                guard.get_opaque().next
            };
            for &(payload, deleted, is_null) in &docs {
                let score = if is_null { f64::NEG_INFINITY } else { 0.0 };
                while matched.get(skip).is_some_and(|&m| m < id) {
                    skip += 1;
                }
                let key = base + u64::from(id);
                if !deleted
                    && matched.get(skip) != Some(&id)
                    && remaining(score, key)
                    && filter(payload)
                {
                    results.push(score, key, payload);
                }
                id += 1;
                check();
            }
            page = next;
        }
        for (i, growing) in prepared.growing.iter().enumerate() {
            // An empty sum is -0.0; zero scores are +0.0.
            let score = if growing.score == f64::NEG_INFINITY {
                f64::NEG_INFINITY
            } else {
                0.0
            };
            let key = i as u64;
            if growing.score <= 0.0 && remaining(score, key) && filter(growing.payload) {
                results.push(score, key, growing.payload);
            }
            check();
        }
    }
    results.into_sorted_vec()
}

/// Upward-rounded sum of non-negative bounds, exact while the sum is zero.
fn add_up(sum: f64, bound: f64) -> f64 {
    if sum == 0.0 {
        bound
    } else {
        (sum + bound).next_up()
    }
}

#[derive(PartialEq, Eq)]
struct Entry {
    score: Reverse<Score>,
    key: u64,
    payload: [u16; 3],
}

impl PartialOrd for Entry {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Entry {
    // The payload is a function of the key.
    fn cmp(&self, other: &Self) -> Ordering {
        (self.score, self.key).cmp(&(other.score, other.key))
    }
}

struct Results {
    limit: NonZero<usize>,
    threshold: Score,
    internal: BinaryHeap<Entry>,
}

impl Results {
    fn new(limit: NonZero<usize>) -> Self {
        Self {
            limit,
            threshold: Score::ZERO,
            internal: BinaryHeap::with_capacity(limit.get().min(4096) + 1),
        }
    }
    fn threshold(&self) -> f64 {
        self.threshold.to_f64()
    }
    fn full(&self) -> bool {
        self.internal.len() >= self.limit.get()
    }
    fn push(&mut self, score: f64, key: u64, payload: [u16; 3]) {
        self.internal.push(Entry {
            score: Reverse(Score::from_f64(score)),
            key,
            payload,
        });
        if self.internal.len() > self.limit.get() {
            self.internal.pop();
        }
        if self.internal.len() == self.limit.get() {
            self.threshold = self.threshold.max(self.internal.peek().unwrap().score.0);
        }
    }
    fn into_sorted_vec(self) -> Vec<(f64, u64, [u16; 3])> {
        self.internal
            .into_sorted_vec()
            .into_iter()
            .map(|e| (e.score.0.to_f64(), e.key, e.payload))
            .collect()
    }
}

struct Cursor {
    bm25: Cache,
    bound: Cache,
    bound_scale: f64,
    exact: bool,
    ordinal: usize,
    token_upper_bound: f64,

    document_id: u32,
    position_in_block: u8,

    incoming: TruncatedTapeReader<Summary>,

    summary: Summary,
    block_upper_bound: f64,
    tight_block_upper_bound: Option<f64>,
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
    #[allow(clippy::too_many_arguments)]
    fn new<R: RelationRead>(
        index: &R,
        token_number_of_documents: u32,
        token_wand_fieldnorm: u8,
        token_wand_term_frequency: u32,
        wptr_summaries: (u32, u16),
        bm25: Cache,
        bound: Cache,
        bound_scale: f64,
        exact: bool,
        ordinal: usize,
    ) -> Self
    where
        R::Page: Page<Opaque = Opaque>,
    {
        let token_upper_bound = upper_bound(
            &bound,
            bound_scale,
            exact,
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
            exact,
            summary.wand_fieldnorm,
            summary.wand_term_frequency,
        );
        Cursor {
            bm25,
            bound,
            bound_scale,
            exact,
            ordinal,
            token_upper_bound,
            document_id: summary.min_document_id,
            position_in_block: 0,
            summary,
            block_upper_bound,
            tight_block_upper_bound: None,
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
    /// The query score of the block's stored pair: documents sharing it score
    /// exactly this, so no bound of the block is lower.
    fn block_pair_score(&self) -> f64 {
        if self.summary.wand_term_frequency == 0 {
            0.0
        } else {
            self.bm25.evaluate(
                self.summary.wand_fieldnorm,
                self.summary.wand_term_frequency,
            )
        }
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
            self.exact,
            self.summary.wand_fieldnorm,
            self.summary.wand_term_frequency,
        );
        self.tight_block_upper_bound = None;
        self.filled = false;
    }
    /// An upper bound of this block under the query's statistics, from its
    /// term frequencies. Flush stored the pair maximizing the sealed `tf`, so
    /// every document with term frequency `f` has a sealed `tf` at most the
    /// pair's, and since `tf` is non-increasing in the fieldnorm, a fieldnorm
    /// no smaller than the least one that satisfies it. Scores are monotone in
    /// the fieldnorm too, so that least fieldnorm bounds them; documents that
    /// share the pair get an exact bound, without any length-drift scaling.
    fn tight_block_upper_bound<R: RelationRead>(&mut self, index: &R) -> f64
    where
        R::Page: Page<Opaque = Opaque>,
    {
        if self.exact {
            return self.block_upper_bound;
        }
        if let Some(bound) = self.tight_block_upper_bound {
            return bound;
        }
        if !self.filled {
            fill_block(
                &mut self.block,
                index,
                self.summary.min_document_id,
                self.summary.wptr_block,
            );
            self.filled = true;
        }
        let result = tight_bound(
            &self.bound,
            &self.bm25,
            self.summary.wand_fieldnorm,
            self.summary.wand_term_frequency,
            self.block.term_frequencies.as_slice().iter().copied(),
        );
        let bound = result.min(self.block_upper_bound);
        self.tight_block_upper_bound = Some(bound);
        bound
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

/// See `Cursor::tight_block_upper_bound`: `sealed` has the statistics that
/// chose the pair, `query` the scan's.
fn tight_bound(
    sealed: &Cache,
    query: &Cache,
    wand_fieldnorm: u8,
    wand_term_frequency: u32,
    term_frequencies: impl Iterator<Item = u32>,
) -> f64 {
    let pair = sealed.tf(wand_fieldnorm, wand_term_frequency);
    let mut seen = [0_u32; 16];
    let mut distinct = 0;
    let mut result = 0.0f64;
    for f in term_frequencies {
        if seen[..distinct].contains(&f) {
            continue;
        }
        if distinct < seen.len() {
            seen[distinct] = f;
            distinct += 1;
        }
        // The least fieldnorm whose sealed `tf` does not exceed the pair's.
        let (mut lo, mut hi) = (0_u16, 256_u16);
        while lo < hi {
            let mid = (lo + hi) / 2;
            if sealed.tf(mid as u8, f) <= pair {
                hi = mid;
            } else {
                lo = mid + 1;
            }
        }
        if lo == 256 {
            return f64::INFINITY;
        }
        result = result.max(query.evaluate(lo as u8, f));
    }
    result
}

// Without length drift each block's WAND pair was chosen by the same
// floating-point function `Cache::evaluate` is monotone in, so it bounds the
// block exactly. With drift, round outward: the real-valued scaling
// inequality must remain an upper bound after evaluation. Zero TF is a
// sentinel.
fn upper_bound(cache: &Cache, scale: f64, exact: bool, fieldnorm: u8, tf: u32) -> f64 {
    if tf == 0 {
        0.0
    } else if exact {
        cache.evaluate(fieldnorm, tf)
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
                        let build = Cache::new(10000, 3000, k1, Cache::norms(k1, b, avg_build));
                        let query = Cache::new(10000, 3000, k1, Cache::norms(k1, b, avg_query));
                        let bound = upper_bound(
                            &build,
                            drift.max(1.0),
                            false,
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

    /// The block-content bound stays sound under any length drift, and is
    /// exact when every document shares the stored pair's term frequency.
    #[test]
    fn tight_block_bound_covers_every_document_after_length_drift() {
        let mut random = 20261109_u64;
        let mut next = || {
            random = random.wrapping_mul(6364136223846793005).wrapping_add(1);
            (random >> 32) as u32
        };
        for k1 in [1.2, 2.0] {
            for b in [0.0, 0.75, 1.0] {
                for drift in [0.001, 0.5, 0.97, 1.03, 2.0, 1000.0] {
                    for uniform in [false, true] {
                        for _ in 0..300 {
                            let avg_build =
                                f64::from(next() % 1000 + 1) / f64::from(next() % 7 + 1);
                            let avg_query = avg_build * drift;
                            let tf0 = next() % 4 + 1;
                            let documents = (0..128)
                                .map(|_| {
                                    let tf = if uniform { tf0 } else { next() % 8 + 1 };
                                    ((next() % 200) as u8, tf)
                                })
                                .collect::<Vec<_>>();
                            let mut wand = Wand::new();
                            for &(norm, tf) in &documents {
                                wand.push(norm, tf, k1, b, avg_build);
                            }
                            let n = next() % 100000 + 1;
                            let df = next() % n + 1;
                            let build = Cache::new(n, df, k1, Cache::norms(k1, b, avg_build));
                            let query = Cache::new(n, df, k1, Cache::norms(k1, b, avg_query));
                            let bound = super::tight_bound(
                                &build,
                                &query,
                                wand.fieldnorm(),
                                wand.term_frequency(),
                                documents.iter().map(|&(_, tf)| tf),
                            );
                            let max = documents
                                .iter()
                                .map(|&(norm, tf)| query.evaluate(norm, tf))
                                .fold(0.0, f64::max);
                            assert!(bound >= max, "unsafe: k1={k1} b={b} drift={drift}");
                            if uniform {
                                assert_eq!(bound, max, "inexact: k1={k1} b={b} drift={drift}");
                            }
                        }
                    }
                }
            }
        }
    }

    /// Without drift, the stored pair is an exact bound: no document of the
    /// block scores above it, and its own documents tie it bit for bit.
    #[test]
    fn exact_wand_bound_covers_every_document_without_drift() {
        let mut random = 20261108_u64;
        let mut next = || {
            random = random.wrapping_mul(6364136223846793005).wrapping_add(1);
            (random >> 32) as u32
        };
        for k1 in [0.9, 1.2, 2.0] {
            for b in [0.0, 0.3, 0.75, 1.0] {
                for _ in 0..2000 {
                    let avg = f64::from(next() % 1000 + 1) / f64::from(next() % 7 + 1);
                    let mut wand = Wand::new();
                    let documents = (0..128)
                        .map(|_| ((next() % 64) as u8, next() % 8 + 1))
                        .collect::<Vec<_>>();
                    for &(norm, tf) in &documents {
                        wand.push(norm, tf, k1, b, avg);
                    }
                    let n = next() % 100000 + 1;
                    let cache = Cache::new(n, next() % n + 1, k1, Cache::norms(k1, b, avg));
                    let bound =
                        upper_bound(&cache, 1.0, true, wand.fieldnorm(), wand.term_frequency());
                    let mut attained = false;
                    for (norm, tf) in documents {
                        let score = cache.evaluate(norm, tf);
                        assert!(
                            bound >= score,
                            "unsafe exact bound: k1={k1} b={b} avg={avg}"
                        );
                        attained |= bound == score;
                    }
                    assert!(attained);
                }
            }
        }
    }
}
