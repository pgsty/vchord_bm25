// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU Affero General Public License v3 (AGPLv3).
// See <https://www.gnu.org/licenses/agpl-3.0.html> for details.
//
// Copyright (c) 2025-2026 TensorChord Inc.
// Copyright (c) 2026 PGSTY PTE. LTD.
// Maintained by Ruohang Feng @ PGSTY (rh@vonng.com)

use std::cmp::Ordering;
use std::hash::Hash;

#[derive(Debug, Clone, Copy, Default)]
#[repr(transparent)]
pub struct AlwaysEqual<T>(pub T);

impl<T> PartialEq for AlwaysEqual<T> {
    #[inline(always)]
    fn eq(&self, _: &Self) -> bool {
        true
    }
}

impl<T> Eq for AlwaysEqual<T> {}

#[allow(clippy::non_canonical_partial_ord_impl)]
impl<T> PartialOrd for AlwaysEqual<T> {
    #[inline(always)]
    fn partial_cmp(&self, _: &Self) -> Option<Ordering> {
        Some(Ordering::Equal)
    }
}

impl<T> Ord for AlwaysEqual<T> {
    #[inline(always)]
    fn cmp(&self, _: &Self) -> Ordering {
        Ordering::Equal
    }
}

impl<T> Hash for AlwaysEqual<T> {
    #[inline(always)]
    fn hash<H: std::hash::Hasher>(&self, _: &mut H) {}
}
