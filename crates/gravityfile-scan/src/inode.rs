//! Inode tracking for hardlink deduplication.
//!
//! Uses a countdown DashMap: on first sight we record (nlink - 2) remaining
//! links; each subsequent encounter decrements the counter and removes the
//! entry when exhausted. This bounds memory to the number of *outstanding*
//! hard-link duplicates rather than the full inode set.

use std::collections::HashMap;

use gravityfile_core::InodeInfo;

/// Tracks seen inodes to prevent double-counting hardlinks.
///
/// When a file has multiple hardlinks, we only want to count its size once.
/// This tracker uses a countdown map to track (inode, device) pairs
/// and automatically evicts entries once all expected hard links have been seen.
///
/// Uses a plain `HashMap` since all access occurs from a single thread
/// (the sequential `for entry_result in walker` loop in `collect_entries`).
#[derive(Debug, Default)]
pub struct InodeTracker {
    seen: HashMap<InodeInfo, u32>,
}

impl InodeTracker {
    /// Create a new inode tracker.
    pub fn new() -> Self {
        Self {
            seen: HashMap::new(),
        }
    }

    /// Track an inode. Returns `true` if this is the first time seeing it.
    ///
    /// `nlink` is the hard-link count from file metadata. When `nlink <= 1`
    /// the file cannot have any additional hard links so we return `true`
    /// immediately without touching the map (no memory allocated).
    ///
    /// On the first encounter the entry is inserted with a countdown equal to
    /// `nlink - 1` — the number of duplicate encounters we still need to
    /// suppress. Each subsequent encounter decrements the counter; when it
    /// reaches zero the entry is removed, bounding memory to the number of
    /// in-flight duplicate hard links.
    pub fn track(&mut self, info: InodeInfo, nlink: u64) -> bool {
        use std::collections::hash_map::Entry;

        if nlink <= 1 {
            return true;
        }

        match self.seen.entry(info) {
            Entry::Vacant(e) => {
                // First time we see this inode. Record how many duplicate
                // encounters we still expect to suppress: (nlink - 1) remaining
                // hard links after this first one.  We saturate at u32::MAX for
                // pathological cases.
                let remaining = (nlink - 1).min(u32::MAX as u64) as u32;
                e.insert(remaining);
                true
            }
            Entry::Occupied(mut e) => {
                let count = e.get_mut();
                if *count <= 1 {
                    e.remove();
                } else {
                    *count -= 1;
                }
                false
            }
        }
    }

    /// Returns the number of inodes currently being tracked (in-flight duplicates).
    ///
    /// This is the number of entries in the countdown map — i.e. inodes whose
    /// full set of hard links has not yet been encountered.  It reaches zero
    /// once every hard-linked inode has been seen the expected number of times.
    ///
    /// Not part of the stable public API; exposed for integration tests.
    #[doc(hidden)]
    pub fn pending_count(&self) -> usize {
        self.seen.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_track_new_inode_nlink_1() {
        let mut tracker = InodeTracker::new();
        let info = InodeInfo::new(12345, 1);
        // nlink=1 means no hard links; always fresh
        assert!(tracker.track(info, 1));
        assert!(tracker.track(info, 1));
    }

    #[test]
    fn test_track_hardlink_nlink_2() {
        let mut tracker = InodeTracker::new();
        let info = InodeInfo::new(12345, 1);
        // Two hard links: first returns true, second returns false
        assert!(tracker.track(info, 2));
        assert!(!tracker.track(info, 2));
        // Entry should be evicted; next call treats it as fresh again
        assert_eq!(tracker.pending_count(), 0);
    }

    #[test]
    fn test_track_hardlink_nlink_3() {
        let mut tracker = InodeTracker::new();
        let info = InodeInfo::new(99, 2);
        // Three hard links: first is counted, next two are duplicates
        assert!(tracker.track(info, 3));
        assert!(!tracker.track(info, 3));
        assert!(!tracker.track(info, 3));
        assert_eq!(tracker.pending_count(), 0);
    }

    #[test]
    fn test_different_devices() {
        let mut tracker = InodeTracker::new();
        let info1 = InodeInfo::new(12345, 1);
        let info2 = InodeInfo::new(12345, 2); // Same inode, different device

        assert!(tracker.track(info1, 2));
        assert!(tracker.track(info2, 2)); // Different device, so it's new
    }
}
