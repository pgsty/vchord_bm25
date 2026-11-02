// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU Affero General Public License v3 (AGPLv3).
// See <https://www.gnu.org/licenses/agpl-3.0.html> for details.
//
// Copyright (c) 2025-2026 TensorChord Inc.
// Copyright (c) 2026 PGSTY PTE. LTD.
// Maintained by Ruohang Feng @ PGSTY (rh@vonng.com)

use bm25::types::Bm25IndexOptions;
use serde::{Deserialize, Serialize};
use validator::Validate;

#[derive(Debug, Clone, Serialize, Deserialize, Validate)]
#[serde(deny_unknown_fields)]
pub struct Bm25BuildOptions {}

#[allow(clippy::derivable_impls)]
impl Default for Bm25BuildOptions {
    fn default() -> Self {
        Self {}
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, Validate)]
#[serde(deny_unknown_fields)]
pub struct Bm25IndexingOptions {
    #[serde(flatten)]
    #[validate(nested)]
    pub index: Bm25IndexOptions,
    #[serde(default)]
    #[validate(nested)]
    pub build: Bm25BuildOptions,
}
