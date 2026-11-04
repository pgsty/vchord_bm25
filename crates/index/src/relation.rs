// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU Affero General Public License v3 (AGPLv3).
// See <https://www.gnu.org/licenses/agpl-3.0.html> for details.
//
// Copyright (c) 2025-2026 TensorChord Inc.
// Copyright (c) 2026 PGSTY PTE. LTD.
// Maintained by Ruohang Feng @ PGSTY (rh@vonng.com)

use std::ops::{Deref, DerefMut};
use zerocopy::{FromBytes, IntoBytes};

/// # Safety
///
/// * `Opaque` must aligned to 8 bytes.
#[allow(unsafe_code)]
pub unsafe trait Opaque: Copy + Send + Sync + FromBytes + IntoBytes + 'static {
    fn is_deleted(&self) -> bool;
    fn set_deleted(&mut self);
}

pub trait Page: Sized + 'static {
    type Opaque: Opaque;

    #[must_use]
    fn get_opaque(&self) -> &Self::Opaque;
    #[must_use]
    fn get_opaque_mut(&mut self) -> &mut Self::Opaque;
    #[must_use]
    fn len(&self) -> u16;
    #[must_use]
    #[inline]
    fn is_empty(&self) -> bool {
        self.len() == 0
    }
    #[must_use]
    fn get(&self, i: u16) -> Option<&[u8]>;
    #[must_use]
    fn get_mut(&mut self, i: u16) -> Option<&mut [u8]>;
    #[must_use]
    fn alloc(&mut self, data: &[u8]) -> Option<u16>;
    fn free(&mut self, i: u16);
    #[must_use]
    fn freespace(&self) -> u16;
    fn clear(&mut self, opaque: Self::Opaque);
    /// An all-zero page: extended but never initialized (e.g. after a crash).
    #[must_use]
    fn is_new(&self) -> bool;
}

pub trait PageGuard {
    fn id(&self) -> u32;
}

pub trait Relation {
    type Page: Page;
}

pub trait RelationId {
    fn id(&self) -> u32;
}

pub trait RelationReadTypes: Relation {
    type ReadGuard<'b>: PageGuard + Deref<Target = Self::Page>;
}

pub trait RelationRead: RelationReadTypes {
    fn read(&self, id: u32) -> Self::ReadGuard<'_>;
}

pub trait RelationWriteTypes: Relation {
    type WriteGuard<'b>: PageGuard + DerefMut<Target = Self::Page>;
}

pub trait RelationWrite: RelationWriteTypes {
    fn write(&self, id: u32) -> Self::WriteGuard<'_>;
    fn alloc(&self, opaque: <Self::Page as Page>::Opaque) -> Self::WriteGuard<'_>;
    /// Publish a page link and initialize its target in one WAL record.
    /// Releases `left`, retaining the lock on `right` for further appends.
    fn commit_pair<'a>(
        &'a self,
        left: Self::WriteGuard<'a>,
        right: Self::WriteGuard<'a>,
    ) -> Self::WriteGuard<'a>;
    fn free(&self, guard: Self::WriteGuard<'_>);
    fn vacuum(&self);
    /// Number of pages in the main fork.
    fn npages(&self) -> u32;
    /// Report a page as reusable without changing it.
    fn record_free(&self, id: u32);
}

pub trait RelationPrefetch: Relation {
    fn prefetch(&self, id: u32);
}
