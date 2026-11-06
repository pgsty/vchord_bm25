// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU Affero General Public License v3 (AGPLv3).
// See <https://www.gnu.org/licenses/agpl-3.0.html> for details.
//
// Copyright (c) 2025-2026 TensorChord Inc.
// Copyright (c) 2026 PGSTY PTE. LTD.
// Maintained by Ruohang Feng @ PGSTY (rh@vonng.com)

//! A byte-budgeted LRU cache of shared values, with a soft budget.
//!
//! - A hit makes the entry the most recently used one.
//! - Entries whose value is still referenced outside the cache
//!   (`Rc::strong_count > 1`, e.g. held by a running scan) are never evicted.
//! - The [`FLOOR`] most recently used entries are never evicted, so a
//!   statement that alternates between a few keys never thrashes.
//! - Every entry, pinned or not, counts towards the byte total, and a new
//!   entry is counted before eviction runs.
//!
//! The budget is therefore soft: pinned and floor entries may exceed it. With
//! more than [`FLOOR`] keys used in rotation and entries larger than the
//! budget, the least recently used unpinned key is evicted on each insert and
//! has to be recomputed on its next use.

use std::borrow::Borrow;
use std::collections::{BTreeMap, HashMap};
use std::hash::Hash;
use std::rc::Rc;

/// Number of most recently used entries kept regardless of the budget.
pub const FLOOR: usize = 8;

struct Slot<V> {
    value: Rc<V>,
    size: usize,
    stamp: u64,
}

pub struct Lru<K, V> {
    slots: HashMap<K, Slot<V>>,
    order: BTreeMap<u64, K>,
    clock: u64,
    bytes: usize,
}

impl<K, V> Default for Lru<K, V> {
    fn default() -> Self {
        Self {
            slots: HashMap::new(),
            order: BTreeMap::new(),
            clock: 0,
            bytes: 0,
        }
    }
}

impl<K: Hash + Eq + Clone, V> Lru<K, V> {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn len(&self) -> usize {
        self.slots.len()
    }

    pub fn is_empty(&self) -> bool {
        self.slots.is_empty()
    }

    /// Bytes of all cached entries, including pinned and floor entries.
    pub fn bytes(&self) -> usize {
        self.bytes
    }

    pub fn contains<Q>(&self, key: &Q) -> bool
    where
        K: Borrow<Q>,
        Q: Hash + Eq + ?Sized,
    {
        self.slots.contains_key(key)
    }

    fn tick(&mut self) -> u64 {
        self.clock += 1;
        self.clock
    }

    /// Returns the cached value and marks it as the most recently used.
    pub fn get<Q>(&mut self, key: &Q) -> Option<Rc<V>>
    where
        K: Borrow<Q>,
        Q: Hash + Eq + ?Sized,
    {
        let stamp = self.tick();
        let slot = self.slots.get_mut(key)?;
        let owned = self.order.remove(&slot.stamp).expect("inconsistent cache");
        slot.stamp = stamp;
        self.order.insert(stamp, owned);
        Some(slot.value.clone())
    }

    /// Inserts (or replaces) an entry as the most recently used one, then
    /// evicts least recently used, unpinned entries outside the floor until
    /// the total fits `budget` or no such entry is left.
    pub fn insert(&mut self, key: K, value: Rc<V>, size: usize, budget: usize) {
        if let Some(old) = self.slots.remove(&key) {
            self.order.remove(&old.stamp);
            self.bytes -= old.size;
        }
        let stamp = self.tick();
        self.bytes += size;
        self.order.insert(stamp, key.clone());
        self.slots.insert(key, Slot { value, size, stamp });
        self.evict(budget);
    }

    fn evict(&mut self, budget: usize) {
        if self.bytes <= budget {
            return;
        }
        // One pass over the entries older than the floor, oldest first.
        let eligible = self.order.len().saturating_sub(FLOOR);
        let candidates = self
            .order
            .keys()
            .take(eligible)
            .copied()
            .collect::<Vec<_>>();
        for stamp in candidates {
            if self.bytes <= budget {
                break;
            }
            let key = &self.order[&stamp];
            if Rc::strong_count(&self.slots[key].value) > 1 {
                continue;
            }
            let key = self.order.remove(&stamp).expect("inconsistent cache");
            let slot = self.slots.remove(&key).expect("inconsistent cache");
            self.bytes -= slot.size;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Every entry is larger than the budget, the worst case for thrashing.
    const SIZE: usize = 100;
    const BUDGET: usize = 10;

    /// Looks `key` up, inserting it on a miss. Returns whether it was a miss.
    fn access(cache: &mut Lru<u32, u32>, key: u32) -> bool {
        if cache.get(&key).is_some() {
            return false;
        }
        cache.insert(key, Rc::new(key), SIZE, BUDGET);
        true
    }

    #[test]
    fn alternating_up_to_floor_keys_never_recollect() {
        for keys in 2..=FLOOR as u32 {
            let mut cache = Lru::new();
            let mut misses = 0;
            for round in 0..50 {
                for key in 0..keys {
                    if access(&mut cache, key) {
                        misses += 1;
                        assert_eq!(round, 0, "key {key} of {keys} was evicted");
                    }
                }
            }
            assert_eq!(misses, keys as usize);
            assert_eq!(cache.len(), keys as usize);
            assert_eq!(cache.bytes(), keys as usize * SIZE);
        }
    }

    #[test]
    fn hits_refresh_recency() {
        let mut cache = Lru::new();
        for key in 0..FLOOR as u32 {
            access(&mut cache, key);
        }
        // Key 0 is the oldest; a hit makes key 1 the oldest instead.
        assert!(cache.get(&0).is_some());
        access(&mut cache, 100);
        assert!(cache.contains(&0));
        assert!(!cache.contains(&1));
        assert_eq!(cache.len(), FLOOR);
    }

    #[test]
    fn more_keys_than_floor_recollect_in_rotation() {
        // Documented limitation: with FLOOR + 1 keys in rotation and every
        // entry over budget, each access after the first round is a miss.
        let keys = FLOOR as u32 + 1;
        let mut cache = Lru::new();
        let mut misses = 0;
        for _ in 0..10 {
            for key in 0..keys {
                misses += access(&mut cache, key) as usize;
            }
        }
        assert_eq!(misses, 10 * keys as usize);
        assert_eq!(cache.len(), FLOOR);
        assert_eq!(cache.bytes(), FLOOR * SIZE);
    }

    #[test]
    fn entries_within_budget_are_kept() {
        let mut cache = Lru::new();
        for key in 0..100 {
            cache.insert(key, Rc::new(key), 1, 1000);
        }
        assert_eq!(cache.len(), 100);
        assert_eq!(cache.bytes(), 100);
        // The total is brought back under the budget, oldest first.
        cache.insert(1000, Rc::new(1000), 950, 1000);
        assert_eq!(cache.bytes(), 1000);
        assert_eq!(cache.len(), 51);
        assert!(!cache.contains(&49) && cache.contains(&50));
    }

    #[test]
    fn pinned_entries_survive() {
        let mut cache = Lru::new();
        // Held outside the cache, like a scan's prepared statistics.
        let pinned = Rc::new(7_u32);
        cache.insert(7, pinned.clone(), SIZE, BUDGET);
        for round in 0..3 {
            for key in 0..(2 * FLOOR as u32) {
                access(&mut cache, key + 100 * (round + 1));
            }
        }
        assert!(cache.contains(&7));
        assert!(Rc::ptr_eq(&cache.get(&7).unwrap(), &pinned));
        // Pinned entries count towards the total.
        assert_eq!(cache.bytes(), (FLOOR + 1) * SIZE);
        // Once released, the entry is evictable like any other.
        drop(pinned);
        for key in 0..(FLOOR as u32 + 1) {
            access(&mut cache, key + 1000);
        }
        assert!(!cache.contains(&7));
    }

    #[test]
    fn eviction_terminates_when_everything_is_pinned() {
        let mut cache = Lru::new();
        let mut pins = Vec::new();
        for key in 0..1000_u32 {
            let value = Rc::new(key);
            pins.push(value.clone());
            cache.insert(key, value, SIZE, BUDGET);
        }
        assert_eq!(cache.len(), 1000);
        assert_eq!(cache.bytes(), 1000 * SIZE);
        drop(pins);
        // Releasing every pin lets the next insert evict down to the floor.
        cache.insert(1000, Rc::new(1000), SIZE, BUDGET);
        assert_eq!(cache.len(), FLOOR);
    }

    #[test]
    fn new_entry_is_counted_before_eviction() {
        let mut cache = Lru::new();
        for key in 0..FLOOR as u32 {
            cache.insert(key, Rc::new(key), 1, 10);
        }
        assert_eq!(cache.bytes(), FLOOR);
        // FLOOR + 1 entries total 8 + 5 = 13 > 10: the oldest goes.
        cache.insert(100, Rc::new(100), 5, 10);
        assert!(!cache.contains(&0));
        assert_eq!(cache.bytes(), FLOOR - 1 + 5);
    }

    #[test]
    fn replacing_a_key_keeps_the_total_exact() {
        let mut cache = Lru::new();
        cache.insert(1, Rc::new(1), 30, 1000);
        cache.insert(1, Rc::new(2), 50, 1000);
        assert_eq!(cache.len(), 1);
        assert_eq!(cache.bytes(), 50);
        assert_eq!(*cache.get(&1).unwrap(), 2);
    }
}
