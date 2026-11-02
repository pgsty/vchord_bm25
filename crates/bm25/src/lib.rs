// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU Affero General Public License v3 (AGPLv3).
// See <https://www.gnu.org/licenses/agpl-3.0.html> for details.
//
// Copyright (c) 2025-2026 TensorChord Inc.
// Copyright (c) 2026 PGSTY PTE. LTD.
// Maintained by Ruohang Feng @ PGSTY (rh@vonng.com)

mod address_documents;
mod address_tokens;
mod bm25;
mod build;
mod bulkdelete;
mod compression;
mod evaluate;
mod flush;
mod insert;
mod maintain;
mod search;
mod tape;
mod tuples;

pub mod io;
pub mod seed;
pub mod segment;
pub mod types;
pub mod vector;

use zerocopy::{FromBytes, Immutable, IntoBytes, KnownLayout};

pub const WIDTH: usize = 16;

const _: () = assert!(WIDTH <= 32);

#[repr(C, align(8))]
#[derive(Debug, Clone, Copy, PartialEq, FromBytes, IntoBytes, Immutable, KnownLayout)]
pub struct Opaque {
    pub next: u32,
    pub flags: u32,
}

#[allow(unsafe_code)]
unsafe impl index::relation::Opaque for Opaque {
    fn is_deleted(&self) -> bool {
        const B: u32 = 1 << 0;
        self.flags & B != 0
    }
    fn set_deleted(&mut self) {
        const B: u32 = 1 << 0;
        self.flags |= B;
    }
}

pub use build::build;
pub use bulkdelete::bulkdelete;
pub use evaluate::evaluate;
pub use insert::insert;
pub use maintain::maintain;
pub use search::search;
