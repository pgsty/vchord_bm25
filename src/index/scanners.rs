// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU Affero General Public License v3 (AGPLv3).
// See <https://www.gnu.org/licenses/agpl-3.0.html> for details.
//
// Copyright (c) 2025-2026 TensorChord Inc.

use crate::index::fetcher::Fetcher;
use index::relation::{Page, RelationId, RelationRead};
use pgrx::pg_sys::Datum;

pub trait SearchBuilder: 'static {
    type Options;

    type Opfamily;

    type Opaque: Copy;

    fn new<R>(relation: R, opfamily: Self::Opfamily) -> Self
    where
        R: RelationId + RelationRead,
        R::Page: Page<Opaque = Self::Opaque>;

    unsafe fn add(&mut self, strategy: u16, datum: Option<Datum>);

    fn build<'b, R>(
        self,
        relation: R,
        options: Self::Options,
        fetcher: impl Fetcher + 'b,
    ) -> Box<dyn Iterator<Item = (f64, [u16; 3])> + 'b>
    where
        R: RelationRead,
        R::Page: Page<Opaque = Self::Opaque>;
}
