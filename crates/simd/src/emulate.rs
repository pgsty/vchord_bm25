// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU Affero General Public License v3 (AGPLv3).
// See <https://www.gnu.org/licenses/agpl-3.0.html> for details.
//
// Copyright (c) 2025-2026 TensorChord Inc.
// Copyright (c) 2026 PGSTY PTE. LTD.
// Maintained by Ruohang Feng @ PGSTY (rh@vonng.com)

#[cfg(target_arch = "aarch64")]
#[inline]
#[target_feature(enable = "neon")]
pub fn vshlq_n_u16<const N: usize>(
    a: core::arch::aarch64::uint16x8_t,
) -> core::arch::aarch64::uint16x8_t {
    seq_macro::seq!(I in 1..16 {
        match N {
            0 => a,
            #(I => core::arch::aarch64::vshlq_n_u16::<I>(a),)*
            _ => unreachable!(),
        }
    })
}

#[cfg(target_arch = "aarch64")]
#[inline]
#[target_feature(enable = "neon")]
pub fn vshlq_n_u32<const N: usize>(
    a: core::arch::aarch64::uint32x4_t,
) -> core::arch::aarch64::uint32x4_t {
    seq_macro::seq!(I in 1..32 {
        match N {
            0 => a,
            #(I => core::arch::aarch64::vshlq_n_u32::<I>(a),)*
            _ => unreachable!(),
        }
    })
}

#[cfg(target_arch = "aarch64")]
#[inline]
#[target_feature(enable = "neon")]
pub fn vshrq_n_u16<const N: usize>(
    a: core::arch::aarch64::uint16x8_t,
) -> core::arch::aarch64::uint16x8_t {
    seq_macro::seq!(I in 1..16 {
        match N {
            0 => a,
            #(I => core::arch::aarch64::vshrq_n_u16::<I>(a),)*
            _ => unreachable!(),
        }
    })
}

#[cfg(target_arch = "aarch64")]
#[inline]
#[target_feature(enable = "neon")]
pub fn vshrq_n_u32<const N: usize>(
    a: core::arch::aarch64::uint32x4_t,
) -> core::arch::aarch64::uint32x4_t {
    seq_macro::seq!(I in 1..32 {
        match N {
            0 => a,
            #(I => core::arch::aarch64::vshrq_n_u32::<I>(a),)*
            _ => unreachable!(),
        }
    })
}
