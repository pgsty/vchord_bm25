// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU Affero General Public License v3 (AGPLv3).
// See <https://www.gnu.org/licenses/agpl-3.0.html> for details.
//
// Copyright (c) 2025-2026 TensorChord Inc.
// Copyright (c) 2026 PGSTY PTE. LTD.
// Maintained by Ruohang Feng @ PGSTY (rh@vonng.com)

use crate::Opaque;
use index::relation::{Opaque as _, Page, Relation, RelationRead, RelationReadTypes};

/// How a reader reacts to a page that no longer belongs to its generation.
///
/// On a primary, maintenance recycles a generation only after every snapshot
/// that could have copied its addresses is gone, so such a page means
/// corruption. A hot standby query is not covered by the primary's horizon
/// (unless `hot_standby_feedback` is on), so it gets a retryable conflict.
///
/// Tombstones need no conflict of their own: VACUUM marks only heap tuples
/// that it has already pruned, and replaying that prune record resolves
/// conflicts with standby snapshots that could still see them (the reason
/// nbtree's VACUUM records carry no conflict horizon either).
#[derive(Clone, Copy)]
pub struct Reader<'a> {
    pub in_recovery: bool,
    pub conflict: &'a dyn Fn(),
}

impl Reader<'_> {
    pub fn stale(&self) -> ! {
        if self.in_recovery {
            (self.conflict)();
        }
        panic!("data corruption: a page of a published segment was recycled");
    }
}

/// Reads pages of one published segment generation, rejecting recycled pages.
pub struct Checked<'r, R> {
    inner: &'r R,
    generation: u64,
    reader: Reader<'r>,
}

impl<'r, R> Checked<'r, R> {
    pub fn new(inner: &'r R, generation: u64, reader: Reader<'r>) -> Self {
        Self {
            inner,
            generation,
            reader,
        }
    }
}

impl<R: Relation> Relation for Checked<'_, R> {
    type Page = R::Page;
}

impl<R: RelationReadTypes> RelationReadTypes for Checked<'_, R> {
    type ReadGuard<'b> = R::ReadGuard<'b>;
}

impl<R: RelationRead> RelationRead for Checked<'_, R>
where
    R::Page: Page<Opaque = Opaque>,
{
    fn read(&self, id: u32) -> Self::ReadGuard<'_> {
        let guard = R::read(self.inner, id);
        let opaque = *guard.get_opaque();
        if opaque.is_deleted() || opaque.generation > self.generation {
            drop(guard);
            self.reader.stale();
        }
        guard
    }
}

/// Decides whether a reader stops at a growing-segment page.
///
/// A page stamped after the reader's generation was appended after the reader
/// copied the jump tuple. On a primary that is the only way to meet one
/// (recycling waits for every such reader), and the documents on it belong to
/// transactions the reader's snapshot cannot see, so the reader stops there.
pub fn growing_page_is_newer(opaque: Opaque, generation: u64, reader: Reader<'_>) -> bool {
    if opaque.is_deleted() {
        reader.stale();
    }
    if opaque.generation > generation {
        if reader.in_recovery {
            reader.stale();
        }
        return true;
    }
    false
}
