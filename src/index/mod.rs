// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU Affero General Public License v3 (AGPLv3).
// See <https://www.gnu.org/licenses/agpl-3.0.html> for details.
//
// Copyright (c) 2025-2026 TensorChord Inc.
// Copyright (c) 2026 PGSTY PTE. LTD.
// Maintained by Ruohang Feng @ PGSTY (rh@vonng.com)

mod bm25;
mod fetcher;
mod gucs;
mod hook;
mod operators;
mod query;
mod storage;
mod temp;
mod traverse;

pub fn init() {
    gucs::init();
    hook::init();
    bm25::am::init();
}
