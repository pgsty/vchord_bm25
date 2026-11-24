// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU Affero General Public License v3 (AGPLv3).
// See <https://www.gnu.org/licenses/agpl-3.0.html> for details.
//
// Copyright (c) 2026 PGSTY PTE. LTD.
// Maintained by Ruohang Feng @ PGSTY (rh@vonng.com)

use crate::Opaque;
use crate::checked::{Reader, growing_page_is_newer};
use crate::prepared::{Jump, Token};
use crate::tuples::{VectorTuple, VectorTupleReader, WithReader};
use crate::vector::Query;
use index::relation::{Page, RelationRead};

pub(crate) struct Hit {
    pub key: u64,
    pub payload: [u16; 3],
    pub score: f64,
}

struct PageRange {
    page: u32,
    first: u16,
    last: u16,
    key: u64,
    is_null: bool,
}

#[derive(Default)]
pub(crate) struct Candidates {
    pub hits: Vec<Hit>,
    pages: Vec<PageRange>,
    live: u64,
    nulls: u64,
}

#[derive(Default)]
pub(crate) struct Unscored {
    candidates: Candidates,
    matches: Vec<(u64, [u16; 3], u8, usize)>,
    terms: Vec<(usize, u32)>,
}

impl Unscored {
    pub fn score(mut self, tokens: &[Token], check: &mut impl FnMut()) -> Candidates {
        let mut start = 0;
        for (i, (key, payload, norm, end)) in self.matches.into_iter().enumerate() {
            let score = self.terms[start..end]
                .iter()
                .map(|&(q, tf)| tokens[q].bm25.evaluate(norm, tf))
                .sum();
            if score > 0.0 {
                self.candidates.hits.push(Hit {
                    key,
                    payload,
                    score,
                });
            }
            start = end;
            if i % 1024 == 0 {
                check();
            }
        }
        self.candidates
    }
}

#[derive(Clone, Copy)]
pub(crate) struct Boundary {
    pub cut: Option<(u32, u16)>,
    pub count: u64,
}

pub(crate) struct GrowingReader<'a, R, C> {
    pub index: &'a R,
    pub reader: Reader<'a>,
    pub check: &'a mut C,
}

impl<R: RelationRead, C: FnMut()> GrowingReader<'_, R, C>
where
    R::Page: Page<Opaque = Opaque>,
{
    pub fn collect(
        &mut self,
        jump: Jump,
        query: &Query,
        cut: Option<(u32, u16)>,
        mut scan: Option<&mut Unscored>,
        mut stats: impl FnMut(u32, &[(usize, u32)]),
    ) -> Boundary {
        let mut position = (jump.ptr_vectors, jump.ptr_vectors_offset);
        let mut boundary = Boundary {
            cut: None,
            count: 0,
        };
        let mut record = None;
        let mut terms = Vec::new();
        while position.0 != u32::MAX {
            let (page, start) = position;
            let guard = self.index.read(page);
            let opaque = *guard.get_opaque();
            if growing_page_is_newer(opaque, jump.generation, self.reader) {
                break;
            }
            let end = cut
                .filter(|&(id, _)| id == page)
                .map_or_else(|| guard.len(), |(_, end)| end);
            let mut range: Option<PageRange> = None;
            for i in start..=end {
                let tuple = VectorTuple::deserialize_ref(guard.get(i).expect("data corruption"));
                let elements = match tuple {
                    VectorTupleReader::_2(t) => {
                        terms.clear();
                        record = Some((t.fieldnorm(), t.is_null(), 0u32));
                        continue;
                    }
                    VectorTupleReader::_1(t) => t.elements(),
                    VectorTupleReader::_0(t) => t.elements(),
                };
                let (norm, is_null, len) = record.as_mut().expect("incomplete vector");
                for e in elements {
                    *len = len.saturating_add(e.value);
                    if let Ok(q) = query.as_slice().binary_search(&e.key) {
                        terms.push((q, e.value));
                    }
                }
                if let VectorTupleReader::_0(t) = tuple {
                    if !*is_null {
                        stats(*len, &terms);
                    }
                    let key = boundary.count;
                    boundary.count += 1;
                    if let Some(scan) = scan.as_deref_mut() {
                        range
                            .get_or_insert_with(|| PageRange {
                                page,
                                first: i,
                                last: i,
                                key,
                                is_null: *is_null,
                            })
                            .last = i;
                        if !bool::from(t.deleted()) {
                            scan.candidates.live += 1;
                            scan.candidates.nulls += u64::from(*is_null);
                            if !*is_null && !terms.is_empty() {
                                scan.terms.extend_from_slice(&terms);
                                scan.matches
                                    .push((key, t.payload(), *norm, scan.terms.len()));
                            }
                        }
                    }
                    record = None;
                }
            }
            if let (Some(scan), Some(range)) = (scan.as_deref_mut(), range) {
                scan.candidates.pages.push(range);
            }
            drop(guard);
            (self.check)();
            boundary.cut = Some((page, end));
            if cut.is_some_and(|(id, _)| id == page) {
                break;
            }
            position = (opaque.next, 1);
        }
        boundary
    }
}

impl Candidates {
    pub fn bytes(&self) -> usize {
        self.hits.capacity() * size_of::<Hit>() + self.pages.capacity() * size_of::<PageRange>()
    }

    // The caller uses generation-checked reads and may execute SQL in visit.
    pub fn visit_tail<R: RelationRead>(
        &self,
        index: &R,
        from: u64,
        is_null: bool,
        mut visit: impl FnMut(u64, [u16; 3]) -> bool,
        check: &mut impl FnMut(),
    ) where
        R::Page: Page<Opaque = Opaque>,
    {
        let count = if is_null {
            self.nulls
        } else {
            self.live - self.nulls - self.hits.len() as u64
        };
        if count == 0 {
            return;
        }
        let first = self
            .pages
            .partition_point(|p| p.key <= from)
            .saturating_sub(1);
        let mut positive = self.hits.partition_point(|h| h.key < from);
        let mut rows = Vec::new();
        for p in &self.pages[first..] {
            let guard = index.read(p.page);
            let mut key = p.key;
            let mut null = p.is_null;
            rows.clear();
            for i in p.first..=p.last {
                match VectorTuple::deserialize_ref(guard.get(i).expect("data corruption")) {
                    VectorTupleReader::_2(t) => null = t.is_null(),
                    VectorTupleReader::_1(_) => (),
                    VectorTupleReader::_0(t) => {
                        if key >= from {
                            if self.hits.get(positive).is_some_and(|h| h.key == key) {
                                positive += 1;
                            } else if null == is_null && !bool::from(t.deleted()) {
                                rows.push((key, t.payload()));
                            }
                        }
                        key += 1;
                    }
                }
            }
            drop(guard);
            check();
            for &(key, payload) in &rows {
                if !visit(key, payload) {
                    return;
                }
            }
        }
    }
}
