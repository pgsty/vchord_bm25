// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU Affero General Public License v3 (AGPLv3).
// See <https://www.gnu.org/licenses/agpl-3.0.html> for details.
//
// Copyright (c) 2025-2026 TensorChord Inc.
// Copyright (c) 2026 PGSTY PTE. LTD.
// Maintained by Ruohang Feng @ PGSTY (rh@vonng.com)

use index::relation::{
    Opaque, Page, PageGuard, Relation, RelationId, RelationPrefetch, RelationRead,
    RelationReadTypes, RelationWrite, RelationWriteTypes,
};
use std::marker::PhantomData;
use std::mem::{MaybeUninit, offset_of};
use std::ops::{Deref, DerefMut};
use std::ptr::NonNull;

#[repr(C, align(8))]
#[derive(Debug)]
pub struct PostgresPage<O> {
    header: pgrx::pg_sys::PageHeaderData,
    content: [u8; pgrx::pg_sys::BLCKSZ as usize - size_of::<pgrx::pg_sys::PageHeaderData>()],
    _opaque: PhantomData<fn(O) -> O>,
}

// It is a non-guaranteed detection.
// If `PageHeaderData` contains padding bytes, const-eval probably fails.
const _: () = {
    use pgrx::pg_sys::PageHeaderData as T;
    use std::mem::{transmute, zeroed};
    const _ZERO: &[u8; size_of::<T>()] = unsafe { transmute(&zeroed::<T>()) };
};

// Layout checks of header.
const _: () = {
    use pgrx::pg_sys::{MAXIMUM_ALIGNOF, PageHeaderData as T};
    assert!(size_of::<T>() == offset_of!(T, pd_linp));
    assert!(size_of::<T>() % MAXIMUM_ALIGNOF as usize == 0);
};

const _: () = assert!(align_of::<PostgresPage<()>>() == pgrx::pg_sys::MAXIMUM_ALIGNOF as usize);
const _: () = assert!(size_of::<PostgresPage<()>>() == pgrx::pg_sys::BLCKSZ as usize);

impl<O: Opaque> Page for PostgresPage<O> {
    type Opaque = O;
    fn get_opaque(&self) -> &O {
        assert!(self.header.pd_special as usize + size_of::<O>() == size_of::<Self>());
        unsafe { &*((self as *const _ as *const O).byte_add(self.header.pd_special as _)) }
    }
    fn get_opaque_mut(&mut self) -> &mut O {
        assert!(self.header.pd_special as usize + size_of::<O>() == size_of::<Self>());
        unsafe { &mut *((self as *mut _ as *mut O).byte_add(self.header.pd_special as _)) }
    }
    fn len(&self) -> u16 {
        use pgrx::pg_sys::{ItemIdData, PageHeaderData};
        assert!(self.header.pd_lower as usize <= size_of::<Self>());
        assert!(self.header.pd_upper as usize <= size_of::<Self>());
        let lower = self.header.pd_lower as usize;
        let upper = self.header.pd_upper as usize;
        assert!(offset_of!(PageHeaderData, pd_linp) <= lower && lower <= upper);
        ((lower - offset_of!(PageHeaderData, pd_linp)) / size_of::<ItemIdData>()) as u16
    }
    fn get(&self, i: u16) -> Option<&[u8]> {
        use pgrx::pg_sys::{ItemIdData, PageHeaderData};
        if i == 0 {
            return None;
        }
        assert!(self.header.pd_lower as usize <= size_of::<Self>());
        let lower = self.header.pd_lower as usize;
        assert!(offset_of!(PageHeaderData, pd_linp) <= lower);
        let n = ((lower - offset_of!(PageHeaderData, pd_linp)) / size_of::<ItemIdData>()) as u16;
        if i > n {
            return None;
        }
        let iid = unsafe { self.header.pd_linp.as_ptr().add((i - 1) as _).read() };
        let lp_off = iid.lp_off() as usize;
        let lp_len = iid.lp_len() as usize;
        match iid.lp_flags() {
            pgrx::pg_sys::LP_UNUSED => return None,
            pgrx::pg_sys::LP_NORMAL => (),
            pgrx::pg_sys::LP_REDIRECT => unimplemented!(),
            pgrx::pg_sys::LP_DEAD => unimplemented!(),
            _ => unreachable!(),
        }
        assert!(offset_of!(PageHeaderData, pd_linp) <= lp_off);
        assert!(lp_off <= size_of::<Self>());
        assert!(lp_len <= size_of::<Self>());
        assert!(lp_off + lp_len <= size_of::<Self>());
        unsafe {
            let ptr = (self as *const Self).cast::<u8>().add(lp_off as _);
            Some(std::slice::from_raw_parts(ptr, lp_len as _))
        }
    }
    fn get_mut(&mut self, i: u16) -> Option<&mut [u8]> {
        use pgrx::pg_sys::{ItemIdData, PageHeaderData};
        if i == 0 {
            return None;
        }
        assert!(self.header.pd_lower as usize <= size_of::<Self>());
        let lower = self.header.pd_lower as usize;
        assert!(offset_of!(PageHeaderData, pd_linp) <= lower);
        let n = ((lower - offset_of!(PageHeaderData, pd_linp)) / size_of::<ItemIdData>()) as u16;
        if i > n {
            return None;
        }
        let iid = unsafe { self.header.pd_linp.as_ptr().add((i - 1) as _).read() };
        let lp_off = iid.lp_off() as usize;
        let lp_len = iid.lp_len() as usize;
        match iid.lp_flags() {
            pgrx::pg_sys::LP_UNUSED => return None,
            pgrx::pg_sys::LP_NORMAL => (),
            pgrx::pg_sys::LP_REDIRECT => unimplemented!(),
            pgrx::pg_sys::LP_DEAD => unimplemented!(),
            _ => unreachable!(),
        }
        assert!(offset_of!(PageHeaderData, pd_linp) <= lp_off);
        assert!(lp_off <= size_of::<Self>());
        assert!(lp_len <= size_of::<Self>());
        assert!(lp_off + lp_len <= size_of::<Self>());
        unsafe {
            let ptr = (self as *mut Self).cast::<u8>().add(lp_off as _);
            Some(std::slice::from_raw_parts_mut(ptr, lp_len as _))
        }
    }
    fn alloc(&mut self, data: &[u8]) -> Option<u16> {
        unsafe {
            let i = pgrx::pg_sys::PageAddItemExtended(
                (self as *const Self).cast_mut().cast(),
                data.as_ptr().cast_mut().cast(),
                data.len(),
                0,
                0,
            );
            if i == 0 { None } else { Some(i) }
        }
    }
    fn free(&mut self, i: u16) {
        unsafe {
            pgrx::pg_sys::PageIndexTupleDeleteNoCompact((self as *mut Self).cast(), i);
        }
    }
    fn freespace(&self) -> u16 {
        unsafe { pgrx::pg_sys::PageGetFreeSpace((self as *const Self).cast_mut().cast()) as u16 }
    }
    fn clear(&mut self, opaque: O) {
        unsafe {
            page_init(self, opaque);
        }
    }
    fn is_new(&self) -> bool {
        self.header.pd_upper == 0
    }
}

unsafe fn page_init<O: Opaque>(this: *mut PostgresPage<O>, opaque: O) {
    unsafe {
        use pgrx::pg_sys::{BLCKSZ, PageHeaderData, PageInit};
        PageInit(this.cast(), BLCKSZ as usize, size_of::<O>());
        assert_eq!(
            (*this.cast::<PageHeaderData>()).pd_special as usize + size_of::<O>(),
            size_of::<PostgresPage<O>>()
        );
        this.cast::<O>()
            .byte_add(size_of::<PostgresPage<O>>() - size_of::<O>())
            .write(opaque);
    }
}

pub struct PostgresBufferReadGuard<Opaque> {
    buf: i32,
    page: NonNull<PostgresPage<Opaque>>,
    id: u32,
}

impl<Opaque> PageGuard for PostgresBufferReadGuard<Opaque> {
    fn id(&self) -> u32 {
        self.id
    }
}

impl<Opaque> Deref for PostgresBufferReadGuard<Opaque> {
    type Target = PostgresPage<Opaque>;

    fn deref(&self) -> &PostgresPage<Opaque> {
        unsafe { self.page.as_ref() }
    }
}

impl<Opaque> Drop for PostgresBufferReadGuard<Opaque> {
    fn drop(&mut self) {
        if crate::index::pg_cleanup_allowed() {
            unsafe {
                pgrx::pg_sys::UnlockReleaseBuffer(self.buf);
            }
        }
    }
}

pub struct PostgresBufferWriteGuard<O: Opaque> {
    buf: i32,
    page: NonNull<PostgresPage<O>>,
    state: *mut pgrx::pg_sys::GenericXLogState,
    id: u32,
    full_image: bool,
}

impl<O: Opaque> PageGuard for PostgresBufferWriteGuard<O> {
    fn id(&self) -> u32 {
        self.id
    }
}

impl<O: Opaque> Deref for PostgresBufferWriteGuard<O> {
    type Target = PostgresPage<O>;

    fn deref(&self) -> &PostgresPage<O> {
        unsafe { self.page.as_ref() }
    }
}

impl<O: Opaque> DerefMut for PostgresBufferWriteGuard<O> {
    fn deref_mut(&mut self) -> &mut PostgresPage<O> {
        unsafe { self.page.as_mut() }
    }
}

impl<O: Opaque> Drop for PostgresBufferWriteGuard<O> {
    fn drop(&mut self) {
        unsafe {
            if self.state.is_null() {
                // Already committed as part of a multi-page record.
            } else if std::thread::panicking() {
                pgrx::pg_sys::GenericXLogAbort(self.state);
            } else {
                pgrx::pg_sys::GenericXLogFinish(self.state);
            }
            pgrx::pg_sys::UnlockReleaseBuffer(self.buf);
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct PostgresRelation<Opaque> {
    raw: pgrx::pg_sys::Relation,
    _phantom: PhantomData<fn(Opaque) -> Opaque>,
}

impl<Opaque> PostgresRelation<Opaque> {
    pub unsafe fn new(raw: pgrx::pg_sys::Relation) -> Self {
        Self {
            raw,
            _phantom: PhantomData,
        }
    }
}

impl<O: Opaque> Relation for PostgresRelation<O> {
    type Page = PostgresPage<O>;
}

impl<O: Opaque> RelationId for PostgresRelation<O> {
    fn id(&self) -> u32 {
        unsafe {
            let oid = (*self.raw).rd_id;
            debug_assert!(oid != pgrx::pg_sys::Oid::INVALID);
            oid.to_u32()
        }
    }
}

impl<O: Opaque> RelationReadTypes for PostgresRelation<O> {
    type ReadGuard<'a> = PostgresBufferReadGuard<O>;
}

impl<O: Opaque> RelationRead for PostgresRelation<O> {
    fn read(&self, id: u32) -> Self::ReadGuard<'_> {
        assert!(id != u32::MAX, "no such page");
        unsafe {
            use pgrx::pg_sys::{
                BUFFER_LOCK_SHARE, ForkNumber, LockBuffer, ReadBufferExtended, ReadBufferMode,
            };
            let buf = ReadBufferExtended(
                self.raw,
                ForkNumber::MAIN_FORKNUM,
                id,
                ReadBufferMode::RBM_NORMAL,
                std::ptr::null_mut(),
            );
            LockBuffer(buf, BUFFER_LOCK_SHARE as _);
            let page =
                NonNull::new(pgrx::BufferGetPage!(buf).get().cast()).expect("failed to get page");
            PostgresBufferReadGuard { buf, page, id }
        }
    }
}

impl<O: Opaque> RelationWriteTypes for PostgresRelation<O> {
    type WriteGuard<'a> = PostgresBufferWriteGuard<O>;
}

impl<O: Opaque> RelationWrite for PostgresRelation<O> {
    fn discard(&self, mut guard: Self::WriteGuard<'_>) {
        unsafe { pgrx::pg_sys::GenericXLogAbort(guard.state) };
        guard.state = std::ptr::null_mut();
    }
    fn commit_pair<'a>(
        &'a self,
        mut left: Self::WriteGuard<'a>,
        mut right: Self::WriteGuard<'a>,
    ) -> Self::WriteGuard<'a> {
        unsafe {
            use pgrx::pg_sys::*;
            let state = GenericXLogStart(self.raw);
            // Preserve each guard's WAL mode: allocation needs an image;
            // recycling two existing pages needs only their small deltas.
            let flags = |full| {
                if full {
                    GENERIC_XLOG_FULL_IMAGE as _
                } else {
                    0
                }
            };
            let l = GenericXLogRegisterBuffer(state, left.buf, flags(left.full_image));
            let r = GenericXLogRegisterBuffer(state, right.buf, flags(right.full_image));
            std::ptr::copy_nonoverlapping(
                left.page.as_ptr().cast::<u8>(),
                l.cast(),
                BLCKSZ as usize,
            );
            std::ptr::copy_nonoverlapping(
                right.page.as_ptr().cast::<u8>(),
                r.cast(),
                BLCKSZ as usize,
            );
            GenericXLogAbort(left.state);
            GenericXLogAbort(right.state);
            left.state = std::ptr::null_mut();
            right.state = std::ptr::null_mut();
            GenericXLogFinish(state);
            drop(left);
            right.state = GenericXLogStart(self.raw);
            right.full_image = false;
            right.page =
                NonNull::new(GenericXLogRegisterBuffer(right.state, right.buf, 0).cast()).unwrap();
            right
        }
    }
    fn write(&self, id: u32) -> PostgresBufferWriteGuard<O> {
        assert!(id != u32::MAX, "no such page");
        unsafe {
            use pgrx::pg_sys::{
                BUFFER_LOCK_EXCLUSIVE, ForkNumber, GenericXLogRegisterBuffer, GenericXLogStart,
                LockBuffer, ReadBufferExtended, ReadBufferMode,
            };
            let buf = ReadBufferExtended(
                self.raw,
                ForkNumber::MAIN_FORKNUM,
                id,
                ReadBufferMode::RBM_NORMAL,
                std::ptr::null_mut(),
            );
            LockBuffer(buf, BUFFER_LOCK_EXCLUSIVE as _);
            let state = GenericXLogStart(self.raw);
            let page = NonNull::new(
                GenericXLogRegisterBuffer(state, buf, 0).cast::<MaybeUninit<PostgresPage<O>>>(),
            )
            .expect("failed to get page");
            PostgresBufferWriteGuard {
                buf,
                page: page.cast(),
                state,
                id,
                full_image: false,
            }
        }
    }
    fn try_write(&self, id: u32) -> Option<PostgresBufferWriteGuard<O>> {
        assert!(id != u32::MAX, "no such page");
        unsafe {
            use pgrx::pg_sys::{
                ConditionalLockBuffer, GenericXLogRegisterBuffer, GenericXLogStart, ReadBuffer,
                ReleaseBuffer,
            };
            let buf = ReadBuffer(self.raw, id);
            if !ConditionalLockBuffer(buf) {
                ReleaseBuffer(buf);
                return None;
            }
            let state = GenericXLogStart(self.raw);
            let page = NonNull::new(GenericXLogRegisterBuffer(state, buf, 0).cast())
                .expect("failed to get page");
            Some(PostgresBufferWriteGuard {
                buf,
                page,
                state,
                id,
                full_image: false,
            })
        }
    }
    fn alloc(&self, opaque: <Self::Page as Page>::Opaque) -> PostgresBufferWriteGuard<O> {
        unsafe {
            use pgrx::pg_sys::{
                GENERIC_XLOG_FULL_IMAGE, GenericXLogRegisterBuffer, GenericXLogStart,
            };
            let buf = loop {
                use pgrx::pg_sys::{
                    BUFFER_LOCK_UNLOCK, ConditionalLockBuffer, LockBuffer, ReadBuffer,
                    ReleaseBuffer,
                };
                let blkno = pgrx::pg_sys::GetFreeIndexPage(self.raw);
                if blkno == pgrx::pg_sys::InvalidBlockNumber {
                    break None;
                }
                let buf = ReadBuffer(self.raw, blkno);
                if ConditionalLockBuffer(buf) {
                    let page = pgrx::BufferGetPage!(buf);
                    if pgrx::PageIsNew!(page) {
                        break Some(buf);
                    }
                    let page = page.get().cast::<PostgresPage<O>>();
                    if (*page).get_opaque().is_deleted() {
                        break Some(buf);
                    }
                    LockBuffer(buf, BUFFER_LOCK_UNLOCK as _);
                }
                ReleaseBuffer(buf);
            };
            let buf = if let Some(buf) = buf {
                buf
            } else {
                #[cfg(feature = "pg15")]
                {
                    use pgrx::pg_sys::{
                        BUFFER_LOCK_EXCLUSIVE, ExclusiveLock, ForkNumber, LockBuffer,
                        LockRelationForExtension, ReadBufferExtended, ReadBufferMode,
                        UnlockRelationForExtension,
                    };
                    LockRelationForExtension(self.raw, ExclusiveLock as _);
                    let buf = ReadBufferExtended(
                        self.raw,
                        ForkNumber::MAIN_FORKNUM,
                        u32::MAX,
                        ReadBufferMode::RBM_NORMAL,
                        std::ptr::null_mut(),
                    );
                    // Lock the new page before other backends can see it as
                    // an all-zero page (a maintenance sweep reports those free).
                    LockBuffer(buf, BUFFER_LOCK_EXCLUSIVE as _);
                    UnlockRelationForExtension(self.raw, ExclusiveLock as _);
                    buf
                }
                #[cfg(any(feature = "pg16", feature = "pg17", feature = "pg18", feature = "pg19"))]
                {
                    use pgrx::pg_sys::{
                        BufferManagerRelation, ExtendBufferedFlags, ExtendBufferedRel, ForkNumber,
                    };
                    let bmr = BufferManagerRelation {
                        rel: self.raw,
                        smgr: std::ptr::null_mut(),
                        relpersistence: 0,
                    };
                    ExtendBufferedRel(
                        bmr,
                        ForkNumber::MAIN_FORKNUM,
                        std::ptr::null_mut(),
                        ExtendBufferedFlags::EB_LOCK_FIRST as _,
                    )
                }
            };
            let state = GenericXLogStart(self.raw);
            let mut page = NonNull::new(
                GenericXLogRegisterBuffer(state, buf, GENERIC_XLOG_FULL_IMAGE as _)
                    .cast::<MaybeUninit<PostgresPage<O>>>(),
            )
            .expect("failed to get page");
            page_init(page.as_mut().as_mut_ptr(), opaque);
            PostgresBufferWriteGuard {
                buf,
                page: page.cast(),
                state,
                id: pgrx::pg_sys::BufferGetBlockNumber(buf),
                full_image: true,
            }
        }
    }
    fn free(&self, mut guard: Self::WriteGuard<'_>) {
        guard.get_opaque_mut().set_deleted();
        unsafe {
            pgrx::pg_sys::RecordFreeIndexPage(self.raw, guard.id());
        }
    }
    fn vacuum(&self) {
        unsafe {
            pgrx::pg_sys::IndexFreeSpaceMapVacuum(self.raw);
        }
    }
    fn npages(&self) -> u32 {
        unsafe {
            pgrx::pg_sys::RelationGetNumberOfBlocksInFork(
                self.raw,
                pgrx::pg_sys::ForkNumber::MAIN_FORKNUM,
            )
        }
    }
    fn record_free(&self, id: u32) {
        unsafe {
            pgrx::pg_sys::RecordFreeIndexPage(self.raw, id);
        }
    }
}

impl<O: Opaque> RelationPrefetch for PostgresRelation<O> {
    fn prefetch(&self, id: u32) {
        assert!(id != u32::MAX, "no such page");
        unsafe {
            use pgrx::pg_sys::PrefetchBuffer;
            PrefetchBuffer(self.raw, 0, id);
        }
    }
}

// Emulate unstable library feature `box_vec_non_null`.
// See https://github.com/rust-lang/rust/issues/130364.

#[allow(dead_code)]
#[must_use]
fn box_into_non_null<T>(b: Box<T>) -> NonNull<T> {
    NonNull::new(Box::into_raw(b)).unwrap()
}

// Readers detect index pages recycled under them. On a primary that is
// corruption; on a hot standby without `hot_standby_feedback` the primary's
// recycling horizon does not cover the query, so it is a recovery conflict.
pub fn reader() -> bm25::Reader<'static> {
    fn conflict() {
        pgrx::ereport!(
            ERROR,
            pgrx::PgSqlErrorCode::ERRCODE_T_R_SERIALIZATION_FAILURE,
            "canceling statement due to conflict with bm25 index maintenance",
            "Index pages read by this query were recycled by maintenance on the primary. Retry the query, or enable hot_standby_feedback."
        );
    }
    bm25::Reader {
        in_recovery: unsafe { pgrx::pg_sys::RecoveryInProgress() },
        conflict: &conflict,
    }
}

#[cfg(test)]
mod tests {
    use pgrx::pg_sys::{ItemIdData, LP_DEAD, LP_NORMAL, LP_REDIRECT, LP_UNUSED};

    // Known bit patterns of `ItemIdData { lp_off:15, lp_flags:2, lp_len:15 }`
    // as GCC and Clang lay it out: from the least significant bit on
    // little-endian targets, from the most significant bit on big-endian.
    #[cfg(target_endian = "little")]
    fn pattern(off: u32, flags: u32, len: u32) -> u32 {
        off | flags << 15 | len << 17
    }
    #[cfg(target_endian = "big")]
    fn pattern(off: u32, flags: u32, len: u32) -> u32 {
        off << 17 | flags << 15 | len
    }

    fn from_raw(raw: u32) -> ItemIdData {
        // SAFETY: every bit pattern is a valid ItemIdData.
        unsafe { std::mem::transmute::<u32, ItemIdData>(raw) }
    }

    fn raw(iid: ItemIdData) -> u32 {
        // SAFETY: ItemIdData is a 4-byte plain bit-field struct.
        unsafe { std::mem::transmute::<ItemIdData, u32>(iid) }
    }

    #[test]
    fn item_id_bit_patterns() {
        assert_eq!(size_of::<ItemIdData>(), 4);
        for (off, flags, len) in [
            (0, LP_UNUSED, 0),
            (8152, LP_NORMAL, 40),
            (24, LP_NORMAL, 8168),
            (0x7FFF, LP_DEAD, 0x7FFF),
            (0x4001, LP_REDIRECT, 0x0002),
            (0, LP_DEAD, 0),
            (0x7FFF, LP_UNUSED, 0),
            (0, LP_UNUSED, 0x7FFF),
        ] {
            let iid = from_raw(pattern(off, flags, len));
            assert_eq!(
                (iid.lp_off(), iid.lp_flags(), iid.lp_len()),
                (off, flags, len)
            );
            let built = ItemIdData {
                _bitfield_align_1: [],
                _bitfield_1: ItemIdData::new_bitfield_1(off, flags, len),
            };
            assert_eq!(raw(built), pattern(off, flags, len));
        }
        // Little-endian words as PostgreSQL writes them into pd_linp.
        #[cfg(target_endian = "little")]
        {
            // lp_off = 8152, LP_NORMAL, lp_len = 40
            let iid = from_raw(0x0050_9FD8);
            assert_eq!(
                (iid.lp_off(), iid.lp_flags(), iid.lp_len()),
                (8152, LP_NORMAL, 40)
            );
            assert_eq!(from_raw(0x0001_8000).lp_flags(), LP_DEAD);
            assert_eq!(from_raw(0x0000_8000).lp_flags(), LP_NORMAL);
            assert_eq!(from_raw(0x0001_0000).lp_flags(), LP_REDIRECT);
            assert_eq!(from_raw(0xFFFE_7FFF).lp_flags(), LP_UNUSED);
        }
    }
}
