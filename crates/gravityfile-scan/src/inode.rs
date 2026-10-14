//! Scan-lifetime inode tracking for physical-size deduplication.

use gravityfile_core::InodeInfo;
use std::collections::HashSet;

/// Tracks physical identities already counted during one scan.
/// Hard-link counts are mutable hints, not safe eviction criteria. Retain
/// observed multi-link identities until the scan ends. Following directory
/// symlinks also requires tracking single-link files reached through aliases.
#[derive(Debug, Default)]
pub struct InodeTracker {
    seen: HashSet<InodeInfo>,
    track_single_links: bool,
}

impl InodeTracker {
    /// Track hard-linked identities; ordinary single-link files need no entry.
    pub fn new() -> Self {
        Self::default()
    }

    /// Track every identity when traversal may revisit a file through aliases.
    pub(crate) fn with_alias_tracking() -> Self {
        Self {
            track_single_links: true,
            ..Self::default()
        }
    }

    /// Return true only when this identity should contribute physical size.
    /// A previously seen identity remains suppressed even if its link count
    /// subsequently falls to one. This is best-effort accounting, not a snapshot
    /// guarantee against inode reuse or concurrent mutation.
    pub fn track(&mut self, info: InodeInfo, nlink: u64) -> bool {
        if self.track_single_links || nlink > 1 {
            self.seen.insert(info)
        } else {
            !self.seen.contains(&info)
        }
    }

    /// Number of retained identities. Entries remain until this tracker drops.
    #[doc(hidden)]
    pub fn pending_count(&self) -> usize {
        self.seen.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn changing_link_counts_and_extra_aliases_do_not_recount_seen_identity() {
        let mut tracker = InodeTracker::new();
        let info = InodeInfo::new(42, 1);
        assert!(tracker.track(info, 2));
        for count in [2, 1, 0, 3, 2] {
            assert!(!tracker.track(info, count));
        }
        let mut aliases = InodeTracker::with_alias_tracking();
        assert!(aliases.track(info, 1));
        assert!(!aliases.track(info, 1));
    }

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
        // Retain identity so later aliases cannot count this inode again
        assert_eq!(tracker.pending_count(), 1);
    }

    #[test]
    fn test_track_hardlink_nlink_3() {
        let mut tracker = InodeTracker::new();
        let info = InodeInfo::new(99, 2);
        // Three hard links: first is counted, next two are duplicates
        assert!(tracker.track(info, 3));
        assert!(!tracker.track(info, 3));
        assert!(!tracker.track(info, 3));
        assert_eq!(tracker.pending_count(), 1);
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
