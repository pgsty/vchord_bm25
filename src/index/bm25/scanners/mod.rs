// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU Affero General Public License v3 (AGPLv3).
// See <https://www.gnu.org/licenses/agpl-3.0.html> for details.
//
// Copyright (c) 2025-2026 TensorChord Inc.

mod default;

pub use default::DefaultBuilder;

// todo(usamoi): add a fallback scanner

#[derive(Debug)]
pub struct SearchOptions {
    pub limit: u32,
    pub prefilter: bool,
}
