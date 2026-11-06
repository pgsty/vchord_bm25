// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU Affero General Public License v3 (AGPLv3).
// See <https://www.gnu.org/licenses/agpl-3.0.html> for details.
//
// Copyright (c) 2025-2026 TensorChord Inc.
// Copyright (c) 2026 PGSTY PTE. LTD.
// Maintained by Ruohang Feng @ PGSTY (rh@vonng.com)

use bm25::vector::{Document, Element, Query, intern};
use pgrx::pg_sys::WordEntry;
use std::num::Saturating;

#[derive(Debug, Clone)]
pub struct TsVectorOwned {
    entries: Vec<WordEntry>,
    bytes: Vec<u8>,
}

impl TsVectorOwned {
    #[expect(dead_code)]
    #[inline(always)]
    pub fn new(entries: Vec<WordEntry>, bytes: Vec<u8>) -> Self {
        Self { entries, bytes }
    }
}

impl TsVectorOwned {
    #[expect(dead_code)]
    #[inline(always)]
    pub fn as_borrowed(&self) -> TsVectorBorrowed<'_> {
        TsVectorBorrowed {
            entries: &self.entries,
            bytes: &self.bytes,
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct TsVectorBorrowed<'a> {
    entries: &'a [WordEntry],
    bytes: &'a [u8],
}

impl<'a> TsVectorBorrowed<'a> {
    /// `entries` are the `WordEntry` words, `bytes` the lexeme and position
    /// data that follows them (the rest of the datum).
    #[inline(always)]
    pub fn new(entries: &'a [WordEntry], bytes: &'a [u8]) -> Self {
        Self { entries, bytes }
    }

    /// Lexemes and their position counts (`None` if stripped), validated
    /// against the data area.
    pub fn iter(&self) -> impl Iterator<Item = Result<(&'a [u8], Option<u16>), &'static str>> {
        let bytes = self.bytes;
        self.entries.iter().map(move |&entry| lexeme(entry, bytes))
    }
}

/// Size of the lexeme data area of a tsvector datum of `varsize` bytes
/// (header included) with `len` entries.
pub fn layout(varsize: usize, len: usize) -> Result<usize, &'static str> {
    // struct TSVectorData { int32 vl_len_; int32 size; WordEntry entries[]; }
    const DATAHDRSIZE: usize = 8;
    let entries_end = size_of::<u32>()
        .checked_mul(len)
        .and_then(|size| size.checked_add(DATAHDRSIZE))
        .ok_or("the entry count overflows")?;
    varsize
        .checked_sub(entries_end)
        .ok_or("the entries do not fit in the datum")
}

fn lexeme(entry: WordEntry, bytes: &[u8]) -> Result<(&[u8], Option<u16>), &'static str> {
    // WordEntry { uint32 haspos:1, len:11, pos:20; } is a C bit-field whose
    // placement depends on the target; use the generated accessors.
    let start = entry.pos() as usize;
    let end = start + entry.len() as usize;
    let string = bytes.get(start..end).ok_or("a lexeme is out of bounds")?;
    if entry.haspos() == 0 {
        return Ok((string, None));
    }
    // WordEntryPosVector { uint16 npos; WordEntryPos pos[npos]; }, at the
    // next 2-byte boundary of the data area (which is 4-byte aligned).
    let offset = end.next_multiple_of(2);
    let count = bytes
        .get(offset..offset + 2)
        .ok_or("a position count is out of bounds")?;
    let count = u16::from_ne_bytes([count[0], count[1]]);
    let positions_end = offset + 2 + 2 * count as usize;
    if positions_end > bytes.len() {
        return Err("a position array is out of bounds");
    }
    Ok((string, Some(count)))
}

/// Raises an ERROR for a tsvector whose layout is inconsistent.
pub fn malformed(reason: &str) -> ! {
    pgrx::pg_sys::panic::ErrorReport::new(
        pgrx::PgSqlErrorCode::ERRCODE_DATA_CORRUPTED,
        format!("malformed tsvector: {reason}"),
        "tsvector",
    )
    .report(pgrx::pg_sys::elog::PgLogLevel::ERROR);
    unreachable!()
}

impl TsVectorBorrowed<'_> {
    #[expect(dead_code)]
    #[inline(always)]
    pub fn own(&self) -> TsVectorOwned {
        TsVectorOwned {
            entries: self.entries.to_vec(),
            bytes: self.bytes.to_vec(),
        }
    }
}

pub fn cast_tsvector_to_document(seed: &[u8; 32], tsvector: TsVectorBorrowed<'_>) -> Document {
    let mut internal = Vec::new();
    for lexeme in tsvector.iter() {
        let (string, count) = lexeme.unwrap_or_else(|reason| malformed(reason));
        let key = intern(seed, string);
        let Some(count) = count else {
            pgrx::pg_sys::panic::ErrorReport::new(
                pgrx::PgSqlErrorCode::ERRCODE_INVALID_PARAMETER_VALUE,
                "tsvector must have positions",
                "cast_tsvector_to_document",
            )
            .set_hint("Create documents with to_tsvector(); do not strip positional information.")
            .report(pgrx::pg_sys::elog::PgLogLevel::ERROR);
            unreachable!();
        };
        let value: u32 = count.into();
        internal.push(Element { key, value });
    }
    internal.sort_unstable_by(|Element { key: l, .. }, Element { key: r, .. }| Ord::cmp(l, r));
    dedup(&mut internal);
    Document::new(internal)
}

pub fn cast_tsvector_to_query(seed: &[u8; 32], tsvector: TsVectorBorrowed<'_>) -> Query {
    let mut internal = Vec::new();
    for lexeme in tsvector.iter() {
        let (string, _) = lexeme.unwrap_or_else(|reason| malformed(reason));
        let key = intern(seed, string);
        internal.push(key);
    }
    internal.sort_unstable();
    internal.dedup();
    Query::new(internal)
}

fn dedup(internal: &mut Vec<Element>) {
    let n = internal.len();
    let (mut i, mut j) = (0_usize, 0_usize);
    while i < n {
        let mut k = i + 1;
        while k < n && internal[k].key == internal[i].key {
            k += 1;
        }
        let value = internal[i..k]
            .iter()
            .map(|&Element { value, .. }| Saturating(value))
            .sum::<Saturating<u32>>()
            .0;
        internal[j] = Element {
            key: internal[i].key,
            value,
        };
        (i, j) = (k, j + 1);
    }
    internal.truncate(j);
}

#[cfg(test)]
mod tests {
    use super::*;

    type Lexeme<'a> = (&'a [u8], Option<&'a [u16]>);

    fn entry(haspos: bool, len: usize, pos: usize) -> WordEntry {
        WordEntry {
            _bitfield_align_1: [],
            _bitfield_1: WordEntry::new_bitfield_1(haspos as u32, len as u32, pos as u32),
        }
    }

    fn raw(entry: WordEntry) -> u32 {
        // SAFETY: WordEntry is a 4-byte plain bit-field struct.
        unsafe { std::mem::transmute::<WordEntry, u32>(entry) }
    }

    fn from_raw(raw: u32) -> WordEntry {
        // SAFETY: every bit pattern is a valid WordEntry.
        unsafe { std::mem::transmute::<u32, WordEntry>(raw) }
    }

    // Known bit patterns of the C bit-field `haspos:1, len:11, pos:20`, as
    // GCC and Clang lay it out: from the least significant bit on
    // little-endian targets, from the most significant bit on big-endian.
    #[cfg(target_endian = "little")]
    fn pattern(haspos: u32, len: u32, pos: u32) -> u32 {
        haspos | len << 1 | pos << 12
    }
    #[cfg(target_endian = "big")]
    fn pattern(haspos: u32, len: u32, pos: u32) -> u32 {
        haspos << 31 | len << 20 | pos
    }

    #[test]
    fn word_entry_bit_patterns() {
        assert_eq!(size_of::<WordEntry>(), 4);
        for (haspos, len, pos) in [
            (0, 0, 0),
            (1, 1, 0),
            (0, 3, 1),
            (1, 2046, 0),
            (1, 2047, (1 << 20) - 1),
            (0, 6, 82040),
            (1, 1, 0xABCDE),
        ] {
            let decoded = from_raw(pattern(haspos, len, pos));
            assert_eq!(
                (decoded.haspos(), decoded.len(), decoded.pos()),
                (haspos, len, pos)
            );
            assert_eq!(
                raw(entry(haspos != 0, len as _, pos as _)),
                pattern(haspos, len, pos)
            );
        }
        // The byte offset occupies the high bits on little-endian targets.
        #[cfg(target_endian = "little")]
        {
            assert_eq!(from_raw(0x0001_0000).pos(), 0x10);
            assert_eq!(from_raw(0x8000_0000).pos(), 0x80000);
            assert_eq!(from_raw(0x0000_0FFE).len(), 2047);
            assert_eq!(from_raw(0x0000_0FFE).pos(), 0);
            assert_eq!(from_raw(0x0000_0001).haspos(), 1);
        }
    }

    /// Lays out entries and data as PostgreSQL's tsvector input does: each
    /// lexeme, then (2-byte aligned) its position count and positions.
    fn build(lexemes: &[Lexeme<'_>]) -> (Vec<WordEntry>, Vec<u8>) {
        let mut entries = Vec::new();
        let mut bytes = Vec::new();
        for &(lexeme, positions) in lexemes {
            let pos = bytes.len();
            bytes.extend_from_slice(lexeme);
            if let Some(positions) = positions {
                if bytes.len() % 2 == 1 {
                    bytes.push(0);
                }
                bytes.extend_from_slice(&(positions.len() as u16).to_ne_bytes());
                for position in positions {
                    bytes.extend_from_slice(&position.to_ne_bytes());
                }
            }
            entries.push(entry(positions.is_some(), lexeme.len(), pos));
        }
        (entries, bytes)
    }

    fn decode(
        entries: &[WordEntry],
        bytes: &[u8],
    ) -> Result<Vec<(Vec<u8>, Option<u16>)>, &'static str> {
        TsVectorBorrowed::new(entries, bytes)
            .iter()
            .map(|r| r.map(|(s, c)| (s.to_vec(), c)))
            .collect()
    }

    #[test]
    fn header_layout() {
        // An empty tsvector is exactly the 8-byte header.
        assert_eq!(layout(8, 0), Ok(0));
        assert_eq!(layout(12, 1), Ok(0));
        assert_eq!(layout(20, 2), Ok(4));
        assert!(layout(7, 0).is_err());
        assert!(layout(8, 1).is_err());
        assert!(layout(15, 2).is_err());
        assert!(layout(usize::MAX, usize::MAX).is_err());
        assert!(layout(usize::MAX, usize::MAX / 4).is_err());
    }

    #[test]
    fn empty() {
        assert_eq!(decode(&[], &[]), Ok(vec![]));
    }

    #[test]
    fn stripped() {
        let (entries, bytes) = build(&[(b"a", None), (b"bcd", None), (b"ef", None)]);
        assert_eq!(bytes, b"abcdef");
        assert_eq!(
            decode(&entries, &bytes),
            Ok(vec![
                (b"a".to_vec(), None),
                (b"bcd".to_vec(), None),
                (b"ef".to_vec(), None)
            ])
        );
    }

    #[test]
    fn positioned_with_odd_lengths() {
        let (entries, bytes) = build(&[
            (b"abc", Some(&[1, 5, 9])),
            (b"d", Some(&[2])),
            (b"ef", None),
            (b"ghij", Some(&[16383])),
        ]);
        // "abc" + pad + npos + 3 positions, "d" + pad + npos + 1, "ef", ...
        assert_eq!(bytes.len(), 4 + 2 + 6 + 2 + 2 + 2 + 2 + 4 + 2 + 2);
        assert_eq!(
            decode(&entries, &bytes),
            Ok(vec![
                (b"abc".to_vec(), Some(3)),
                (b"d".to_vec(), Some(1)),
                (b"ef".to_vec(), None),
                (b"ghij".to_vec(), Some(1))
            ])
        );
    }

    #[test]
    fn longest_lexeme_and_far_offsets() {
        // MAXSTRLEN - 1 bytes, and offsets beyond 64 KiB (high bits of pos).
        let long = vec![b'x'; 2046];
        let lexemes = (0..40)
            .map(|i| {
                if i % 2 == 0 {
                    (&long[..], Some(&[1_u16, 2][..]))
                } else {
                    (&long[..], None)
                }
            })
            .chain([(&b"needle"[..], Some(&[7_u16, 8, 9][..]))])
            .collect::<Vec<_>>();
        let (entries, bytes) = build(&lexemes);
        assert!(bytes.len() > 1 << 16);
        let decoded = decode(&entries, &bytes).unwrap();
        assert_eq!(decoded.len(), 41);
        assert!(decoded[..40].iter().all(|(s, _)| s.len() == 2046));
        assert_eq!(decoded[39].1, None);
        assert_eq!(decoded[40], (b"needle".to_vec(), Some(3)));
    }

    #[test]
    fn largest_offset() {
        // pos is a 20-bit byte offset.
        let pos = (1 << 20) - 1;
        let mut bytes = vec![0_u8; pos];
        bytes.extend_from_slice(b"z");
        let entries = [entry(false, 1, pos)];
        assert_eq!(decode(&entries, &bytes), Ok(vec![(b"z".to_vec(), None)]));
        assert!(decode(&entries, &bytes[..pos]).is_err());
    }

    #[test]
    fn truncated_or_malformed() {
        let (entries, bytes) = build(&[(b"abc", Some(&[1, 2])), (b"de", None)]);
        assert!(decode(&entries, &bytes).is_ok());
        // Missing the last lexeme's bytes.
        assert_eq!(
            decode(&entries, &bytes[..bytes.len() - 1]),
            Err("a lexeme is out of bounds")
        );
        let (entries, bytes) = build(&[(b"abc", Some(&[1, 2]))]);
        // "abc" pad | npos | pos pos
        assert_eq!(bytes.len(), 10);
        assert_eq!(
            decode(&entries, &bytes[..9]),
            Err("a position array is out of bounds")
        );
        assert_eq!(
            decode(&entries, &bytes[..6]),
            Err("a position array is out of bounds")
        );
        assert_eq!(
            decode(&entries, &bytes[..5]),
            Err("a position count is out of bounds")
        );
        assert_eq!(
            decode(&entries, &bytes[..4]),
            Err("a position count is out of bounds")
        );
        assert_eq!(
            decode(&entries, &bytes[..3]),
            Err("a position count is out of bounds")
        );
        assert_eq!(
            decode(&entries, &bytes[..2]),
            Err("a lexeme is out of bounds")
        );
        // A count larger than the data that follows it.
        let mut bytes = bytes.clone();
        bytes[4..6].copy_from_slice(&3_u16.to_ne_bytes());
        assert_eq!(
            decode(&entries, &bytes),
            Err("a position array is out of bounds")
        );
        // An entry pointing past the data area, and one that only fits by
        // wrapping around.
        assert_eq!(
            decode(&[entry(false, 1, 10)], &[0; 10]),
            Err("a lexeme is out of bounds")
        );
        assert_eq!(
            decode(&[entry(false, 2047, (1 << 20) - 1)], &[0; 10]),
            Err("a lexeme is out of bounds")
        );
        assert!(decode(&[entry(false, 0, 10)], &[0; 10]).is_ok());
    }
}
