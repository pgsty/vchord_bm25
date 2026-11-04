// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU Affero General Public License v3 (AGPLv3).
// See <https://www.gnu.org/licenses/agpl-3.0.html> for details.
//
// Copyright (c) 2025-2026 TensorChord Inc.
// Copyright (c) 2026 PGSTY PTE. LTD.
// Maintained by Ruohang Feng @ PGSTY (rh@vonng.com)

use crate::io::{MappingsWriter, RecordsWriter, handle_io_error};
use crate::segment::{Mapping, Record};
use crate::tape::TapeReader;
use crate::tuples::*;
use crate::vector::Document;
use crate::{Opaque, WIDTH, compression};
use index::relation::{Opaque as _, Page, RelationRead, RelationWrite};
use std::fs::{File, OpenOptions};
use std::io::BufWriter;
use std::path::Path;
use zerocopy::{FromBytes, IntoBytes};

#[derive(Debug, Default)]
pub struct Maintained {
    /// Pages of the previous generation released in this pass.
    pub recycled: u32,
    /// Pages of an unpublished rebuild (cancelled or crashed) released.
    pub orphans: u32,
    pub rebuilt: bool,
    pub live_rows: u64,
}

pub struct Policy {
    pub min_rows: u32,
    pub scale_factor: f64,
    pub force: bool,
}

// Maintenance runs concurrently with readers and inserts. The caller must
// serialize maintainers, e.g. with VACUUM's ShareUpdateExclusiveLock.
//
// A rebuilt segment is published together with `safexid`. Its predecessor's
// pages stay in place until a later pass sees `can_recycle(safexid)`: every
// snapshot that could have copied their addresses is gone by then.
pub fn maintain<R: RelationRead + RelationWrite>(
    index: &R,
    check: impl Fn(),
    dir: &Path,
    file: &Path,
    can_recycle: impl Fn(u64) -> bool,
    next_xid: impl Fn() -> u64,
    policy: Policy,
) -> Maintained
where
    R::Page: Page<Opaque = Opaque>,
{
    let meta_guard = index.read(0);
    let meta_bytes = meta_guard.get(1).expect("data corruption");
    let meta_tuple = MetaTuple::deserialize_ref(meta_bytes);
    let k1 = meta_tuple.k1();
    let b = meta_tuple.b();
    let ptr_jump = meta_tuple.ptr_jump();
    drop(meta_guard);

    let (generation, safexid) = {
        let jump_guard = index.read(ptr_jump);
        let jump_bytes = jump_guard.get(1).expect("data corruption");
        let jump_tuple = JumpTuple::deserialize_ref(jump_bytes);
        (jump_tuple.generation(), jump_tuple.safexid())
    };

    let mut maintained = Maintained::default();
    let mut pending = safexid != 0;
    if pending && can_recycle(safexid) {
        maintained.recycled = recycle(index, ptr_jump, &check);
        pending = false;
    }
    // While a generation is pending, its pages may already be marked deleted
    // by an interrupted recycle; they must not reach the FSM before their
    // chain head in the jump tuple has moved past them.
    maintained.orphans = sweep(index, generation, !pending, &check);
    index.vacuum();
    let (old, cut, sealed, growing, deleted) = inspect(index, ptr_jump, &check);
    maintained.live_rows = sealed + growing - deleted;
    let growth_threshold = f64::from(policy.min_rows).max(policy.scale_factor * sealed as f64);
    let changed = growing != 0 || deleted != 0;
    let enough = (growing != 0 && growing as f64 >= growth_threshold)
        || (deleted != 0 && deleted as f64 >= policy.scale_factor * (sealed + growing) as f64);
    if pending || !changed || !(policy.force || enough) {
        return maintained;
    }
    rebuild(
        index,
        &check,
        dir,
        file,
        (k1, b),
        ptr_jump,
        generation,
        old,
        cut,
        &next_xid,
    );
    maintained.rebuilt = true;
    index.vacuum();
    maintained
}

const RECYCLE_BATCH: usize = 64;
const GRAVEYARD_CHAINS: usize = 7;

fn graveyard_get(jump_tuple: JumpTupleReader<'_>, chain: usize) -> (u32, u32) {
    let end = u32::MAX;
    match chain {
        0 => (
            jump_tuple.graveyard_vectors(),
            jump_tuple.graveyard_vectors_end(),
        ),
        1 => (jump_tuple.graveyard_documents(), end),
        2 => (jump_tuple.graveyard_tokens(), end),
        3 => (jump_tuple.graveyard_summaries(), end),
        4 => (jump_tuple.graveyard_blocks(), end),
        5 => (jump_tuple.graveyard_free_documents(), end),
        6 => (jump_tuple.graveyard_free_tokens(), end),
        _ => unreachable!(),
    }
}

fn graveyard_set(jump_tuple: &mut JumpTupleWriter<'_>, chain: usize, value: u32) {
    *match chain {
        0 => jump_tuple.graveyard_vectors(),
        1 => jump_tuple.graveyard_documents(),
        2 => jump_tuple.graveyard_tokens(),
        3 => jump_tuple.graveyard_summaries(),
        4 => jump_tuple.graveyard_blocks(),
        5 => jump_tuple.graveyard_free_documents(),
        6 => jump_tuple.graveyard_free_tokens(),
        _ => unreachable!(),
    } = value;
}

// Releases the previous generation. Each batch is marked deleted (keeping
// `next`), then the chain head in the jump tuple moves past it, and only
// then do the pages enter the FSM. A crash at any point leaves either pages
// that are revisited through their intact `next`, or deleted pages that the
// next sweep reports to the FSM.
fn recycle<R: RelationRead + RelationWrite>(index: &R, ptr_jump: u32, check: &impl Fn()) -> u32
where
    R::Page: Page<Opaque = Opaque>,
{
    let mut recycled = 0_u32;
    for chain in 0..GRAVEYARD_CHAINS {
        loop {
            let (mut current, end) = {
                let jump_guard = index.read(ptr_jump);
                let jump_bytes = jump_guard.get(1).expect("data corruption");
                graveyard_get(JumpTuple::deserialize_ref(jump_bytes), chain)
            };
            if current == end || current == u32::MAX {
                break;
            }
            let mut batch = Vec::with_capacity(RECYCLE_BATCH);
            while batch.len() < RECYCLE_BATCH && current != end && current != u32::MAX {
                let mut guard = index.write(current);
                let next = guard.get_opaque().next;
                if !guard.get_opaque().is_deleted() {
                    guard.get_opaque_mut().set_deleted();
                }
                drop(guard);
                batch.push(current);
                current = next;
            }
            {
                let mut jump_guard = index.write(ptr_jump);
                let jump_bytes = jump_guard.get_mut(1).expect("data corruption");
                graveyard_set(&mut JumpTuple::deserialize_mut(jump_bytes), chain, current);
            }
            for &id in batch.iter() {
                index.record_free(id);
            }
            recycled += batch.len() as u32;
            check();
        }
    }
    let mut jump_guard = index.write(ptr_jump);
    let jump_bytes = jump_guard.get_mut(1).expect("data corruption");
    let mut jump_tuple = JumpTuple::deserialize_mut(jump_bytes);
    for chain in 0..GRAVEYARD_CHAINS {
        graveyard_set(&mut jump_tuple, chain, u32::MAX);
    }
    *jump_tuple.graveyard_vectors_end() = u32::MAX;
    *jump_tuple.safexid() = 0;
    recycled
}

// Scans every page. Pages stamped after the published generation belong to a
// rebuild that never published (inserts stamp the published generation), so
// nothing references them. With `rebuild_fsm`, also reports deleted and
// never-initialized pages, so a free-space map lost in a crash recovers.
fn sweep<R: RelationRead + RelationWrite>(
    index: &R,
    generation: u64,
    rebuild_fsm: bool,
    check: &impl Fn(),
) -> u32
where
    R::Page: Page<Opaque = Opaque>,
{
    let mut orphans = 0_u32;
    for id in 1..index.npages() {
        let (reusable, orphan) = {
            let guard = index.read(id);
            if guard.is_new() {
                (true, false)
            } else {
                let opaque = *guard.get_opaque();
                let deleted = opaque.is_deleted();
                (deleted, !deleted && opaque.generation > generation)
            }
        };
        if orphan {
            let guard = index.write(id);
            let opaque = *guard.get_opaque();
            if !opaque.is_deleted() && opaque.generation > generation {
                index.free(guard);
                orphans += 1;
            }
        } else if reusable && rebuild_fsm {
            index.record_free(id);
        }
        if id % 128 == 0 {
            check();
        }
    }
    orphans
}

// Only active chains contribute to counts; retired generations and abandoned
// output are not part of the current corpus. Capture the append boundary first.
fn inspect<R: RelationRead>(
    index: &R,
    ptr_jump: u32,
    check: &impl Fn(),
) -> (OldSegment, (u32, u16), u64, u64, u64)
where
    R::Page: Page<Opaque = Opaque>,
{
    let (old, mut tail, sealed) = {
        let guard = index.read(ptr_jump);
        let j = JumpTuple::deserialize_ref(guard.get(1).expect("data corruption"));
        (
            OldSegment {
                ptr_vectors: j.ptr_vectors(),
                ptr_vectors_offset: j.ptr_vectors_offset(),
                ptr_documents: j.ptr_documents(),
                ptr_tokens: j.ptr_tokens(),
                ptr_summaries: j.ptr_summaries(),
                ptr_blocks: j.ptr_blocks(),
                free_documents: j.free_documents(),
                free_tokens: j.free_tokens(),
            },
            j.tail_hint(),
            u64::from(j.number_of_documents()),
        )
    };
    let cut = loop {
        let guard = index.read(tail);
        let next = guard.get_opaque().next;
        let end = guard.len() + 1;
        drop(guard);
        check();
        if next == u32::MAX {
            break (tail, end);
        }
        tail = next;
    };
    let mut deleted = 0;
    let mut page = old.ptr_documents;
    while page != u32::MAX {
        let guard = index.read(page);
        for i in 1..=guard.len() {
            let t = DocumentTuple::deserialize_ref(guard.get(i).expect("data corruption"));
            deleted += u64::from(bool::from(t.deleted()));
        }
        page = guard.get_opaque().next;
        drop(guard);
        check();
    }
    let mut growing = 0;
    let mut page = old.ptr_vectors;
    let mut start = old.ptr_vectors_offset;
    loop {
        let guard = index.read(page);
        let end = if page == cut.0 {
            cut.1 - 1
        } else {
            guard.len()
        };
        for i in start..=end {
            if let VectorTupleReader::_0(t) =
                VectorTuple::deserialize_ref(guard.get(i).expect("data corruption"))
            {
                growing += 1;
                deleted += u64::from(bool::from(t.deleted()));
            }
        }
        let next = guard.get_opaque().next;
        drop(guard);
        check();
        if page == cut.0 {
            break;
        }
        page = next;
        start = 1;
    }
    (old, cut, sealed, growing, deleted)
}

#[allow(clippy::too_many_arguments)]
fn rebuild<R: RelationRead + RelationWrite>(
    index: &R,
    check: &impl Fn(),
    dir: &Path,
    file: &Path,
    (k1, b): (f64, f64),
    ptr_jump: u32,
    generation: u64,
    old: OldSegment,
    cut: (u32, u16),
    next_xid: &impl Fn() -> u64,
) where
    R::Page: Page<Opaque = Opaque>,
{
    let mut relabel = BufWriter::with_capacity(
        16 * 1024,
        handle_io_error(OpenOptions::new().read(true).write(true).open(file)),
    );
    let mut records_writer = crate::io::records_writer(dir, 0);

    {
        let first = old.ptr_documents;
        assert!(first != u32::MAX);
        let mut current = first;
        while current != u32::MAX {
            let next = {
                let guard = index.read(current);
                for i in 1..=guard.len() {
                    let bytes = guard.get(i).expect("data corruption");
                    let tuple = DocumentTuple::deserialize_ref(bytes);
                    add_document(
                        &mut relabel,
                        &mut records_writer,
                        (!bool::from(tuple.deleted())).then_some(tuple.payload()),
                        tuple.is_null(),
                    );
                }
                guard.get_opaque().next
            };
            check();
            current = next;
        }
    }

    let relabel = handle_io_error(relabel.into_inner().map_err(|e| e.into_error()));
    let relabel_memmap = if handle_io_error(relabel.metadata()).len() != 0 {
        #[allow(unsafe_code)]
        Some(unsafe { handle_io_error(memmap2::Mmap::map(&relabel)) })
    } else {
        None
    };
    let relabel_slice: &[u32] = if let Some(memmap) = relabel_memmap.as_ref() {
        FromBytes::ref_from_bytes(memmap).expect("failed to read memory map")
    } else {
        &[]
    };
    records_writer.flush();
    let mut records_memmap = {
        let file = records_writer.get_ref();
        if handle_io_error(file.metadata()).len() != 0 {
            #[allow(unsafe_code)]
            Some(unsafe { handle_io_error(memmap2::MmapMut::map_mut(file)) })
        } else {
            None
        }
    };
    let records_slice: &mut [Record] = if let Some(memmap) = records_memmap.as_mut() {
        FromBytes::mut_from_bytes(memmap.as_mut()).expect("failed to read memory map")
    } else {
        &mut []
    };
    let mut mappings_writer = crate::io::mappings_writer(dir, 0);

    {
        let mut tape_tokens = TapeReader::new(old.ptr_tokens, |bytes| {
            let token_tuple = TokenTuple::deserialize_ref(bytes);
            Token {
                id: token_tuple.id(),
                number_of_documents: token_tuple.number_of_documents(),
            }
        });
        let mut tape_summaries = TapeReader::new(old.ptr_summaries, |bytes| {
            let summary_tuple = SummaryTuple::deserialize_ref(bytes);
            Summary {
                min_document_id: summary_tuple.min_document_id(),
                number_of_documents: summary_tuple.number_of_documents(),
            }
        });
        let mut tape_blocks = TapeReader::new(old.ptr_blocks, |bytes| {
            let block_tuple = BlockTuple::deserialize_ref(bytes);
            Block {
                metadata_document_ids: block_tuple.metadata_document_ids(),
                compressed_document_ids: block_tuple.compressed_document_ids().to_vec(),
                metadata_term_frequencies: block_tuple.metadata_term_frequencies(),
                compressed_term_frequencies: block_tuple.compressed_term_frequencies().to_vec(),
            }
        });
        while let Some(token) = tape_tokens.next(index) {
            for block_no in 0..token.number_of_documents.div_ceil(128) {
                if block_no % 128 == 0 {
                    check();
                }
                let summary = tape_summaries.next(index).expect("data corruption");
                let block = tape_blocks.next(index).expect("data corruption");
                let mut document_ids = compression::Decompressed::new();
                compression::decompress_document_ids(
                    summary.min_document_id,
                    block.metadata_document_ids,
                    &block.compressed_document_ids,
                    &mut document_ids,
                );
                let mut term_frequencies = compression::Decompressed::new();
                compression::decompress_term_frequencies(
                    block.metadata_term_frequencies,
                    &block.compressed_term_frequencies,
                    &mut term_frequencies,
                );
                for i in 0..summary.number_of_documents {
                    let document_id = document_ids.as_slice()[i as usize];
                    let term_frequency = term_frequencies.as_slice()[i as usize];
                    add_element(
                        relabel_slice,
                        records_slice,
                        &mut mappings_writer,
                        token.id,
                        document_id,
                        term_frequency,
                    );
                }
            }
            check();
        }
        assert!(tape_summaries.next(index).is_none(), "data corruption");
        assert!(tape_blocks.next(index).is_none(), "data corruption");
    }

    drop(records_memmap);
    drop(relabel_memmap);
    drop(relabel);

    // Take the growing segment up to its current end without modifying the
    // chain: inserts keep appending after the cut, and an interrupted rebuild
    // leaves the chain exactly as it was. A multi-page document cannot cross
    // the cut, because its inserter holds the tail page while appending.
    {
        let mut state = None;
        let mut current = old.ptr_vectors;
        let mut start = old.ptr_vectors_offset;
        loop {
            assert!(current != u32::MAX);
            let (next, documents) = {
                let guard = index.read(current);
                let mut documents = Vec::new();
                let end = if current == cut.0 {
                    cut.1 - 1
                } else {
                    guard.len()
                };
                for i in start..=end {
                    let vector_bytes = guard.get(i).expect("data corruption");
                    let vector_tuple = VectorTuple::deserialize_ref(vector_bytes);
                    match vector_tuple {
                        VectorTupleReader::_2(t) => {
                            state = Some((t.is_null(), Vec::new()));
                        }
                        VectorTupleReader::_1(vector_tuple) => {
                            if let Some((_, internal)) = state.as_mut() {
                                internal.extend(vector_tuple.elements());
                            } else {
                                panic!("data corruption");
                            }
                        }
                        VectorTupleReader::_0(vector_tuple) => {
                            if let Some((is_null, mut internal)) = state.take() {
                                if !bool::from(vector_tuple.deleted()) {
                                    internal.extend(vector_tuple.elements());
                                    let document = if is_null {
                                        Document::null()
                                    } else {
                                        Document::new(internal)
                                    };
                                    documents.push((document, vector_tuple.payload()));
                                }
                            } else {
                                panic!("data corruption");
                            }
                        }
                    }
                }
                (guard.get_opaque().next, documents)
            };
            for (document, payload) in documents {
                crate::io::write(
                    &mut records_writer,
                    &mut mappings_writer,
                    &document,
                    payload,
                );
            }
            check();
            if current == cut.0 {
                break;
            }
            current = next;
            start = 1;
        }
    };

    records_writer.flush();
    mappings_writer.flush();
    drop(records_writer);
    drop(mappings_writer);
    crate::io::locally_merge(dir, 0, check);

    let segment = crate::io::readers(dir, 1);
    let flushed = crate::flush::flush(k1, b, index, segment, generation + 1, check);
    check();

    let mut jump_guard = index.write(ptr_jump);
    let jump_bytes = jump_guard.get_mut(1).expect("data corruption");
    let mut jump_tuple = JumpTuple::deserialize_mut(jump_bytes);
    assert_eq!(
        *jump_tuple.generation(),
        generation,
        "concurrent maintenance"
    );
    assert_eq!(
        *jump_tuple.safexid(),
        0,
        "previous generation is not recycled"
    );

    *jump_tuple.graveyard_vectors() = if old.ptr_vectors != cut.0 {
        old.ptr_vectors
    } else {
        u32::MAX
    };
    *jump_tuple.graveyard_vectors_end() = cut.0;
    *jump_tuple.graveyard_documents() = old.ptr_documents;
    *jump_tuple.graveyard_tokens() = old.ptr_tokens;
    *jump_tuple.graveyard_summaries() = old.ptr_summaries;
    *jump_tuple.graveyard_blocks() = old.ptr_blocks;
    *jump_tuple.graveyard_free_documents() = old.free_documents;
    *jump_tuple.graveyard_free_tokens() = old.free_tokens;

    *jump_tuple.ptr_vectors() = cut.0;
    *jump_tuple.ptr_vectors_offset() = cut.1;
    *jump_tuple.number_of_documents() = flushed.number_of_documents;
    *jump_tuple.number_of_nulls() = flushed.number_of_nulls;
    // Reset to a known retained page; the next inserter advances this hint
    // across any suffix appended concurrently with the rebuild.
    *jump_tuple.tail_hint() = cut.0;
    *jump_tuple.sum_of_document_lengths() = flushed.sum_of_document_lengths;
    *jump_tuple.width_1_documents() = flushed.width_1_documents;
    *jump_tuple.width_0_documents() = flushed.width_0_documents;
    *jump_tuple.depth_documents() = flushed.depth_documents;
    *jump_tuple.start_documents() = flushed.start_documents;
    *jump_tuple.free_documents() = flushed.free_documents;
    *jump_tuple.depth_tokens() = flushed.depth_tokens;
    *jump_tuple.start_tokens() = flushed.start_tokens;
    *jump_tuple.free_tokens() = flushed.free_tokens;
    *jump_tuple.ptr_documents() = flushed.ptr_documents;
    *jump_tuple.ptr_tokens() = flushed.ptr_tokens;
    *jump_tuple.ptr_summaries() = flushed.ptr_summaries;
    *jump_tuple.ptr_blocks() = flushed.ptr_blocks;
    *jump_tuple.generation() = generation + 1;
    // Read the horizon only while holding the jump page: a reader can no
    // longer copy the old addresses, and every reader that did took its
    // snapshot (hence its xmin) before this point.
    *jump_tuple.safexid() = next_xid();
}

struct OldSegment {
    ptr_vectors: u32,
    ptr_vectors_offset: u16,
    ptr_documents: u32,
    ptr_tokens: u32,
    ptr_summaries: u32,
    ptr_blocks: u32,
    free_documents: u32,
    free_tokens: u32,
}

struct Token {
    id: [u8; WIDTH],
    number_of_documents: u32,
}

struct Summary {
    min_document_id: u32,
    number_of_documents: u8,
}

struct Block {
    metadata_document_ids: u8,
    compressed_document_ids: Vec<u8>,
    metadata_term_frequencies: u8,
    compressed_term_frequencies: Vec<u8>,
}

fn add_document(
    relabel: &mut BufWriter<File>,
    records_writer: &mut RecordsWriter,
    payload: Option<[u16; 3]>,
    is_null: bool,
) {
    use std::io::Write;
    let label = if let Some(payload) = payload {
        records_writer.write(Record(0_u32, payload, u16::from(is_null)))
    } else {
        u32::MAX
    };
    handle_io_error(relabel.write_all(label.as_bytes()));
}

fn add_element(
    relabel_slice: &[u32],
    records_slice: &mut [Record],
    mappings_writer: &mut MappingsWriter,
    token_id: [u8; WIDTH],
    document_id: u32,
    term_frequency: u32,
) {
    let document_id = {
        let label = relabel_slice[document_id as usize];
        if label != u32::MAX { label } else { return }
    };
    {
        let Record(mut length, payload, is_null) = records_slice[document_id as usize];
        // Document length counts every occurrence, not only distinct tokens.
        // Using one here changes field norms and ranking after VACUUM.
        length = length.saturating_add(term_frequency);
        records_slice[document_id as usize] = Record(length, payload, is_null);
    }
    mappings_writer.write(Mapping(token_id, document_id, term_frequency));
}
