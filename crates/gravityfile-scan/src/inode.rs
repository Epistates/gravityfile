//! Inode tracking for hardlink deduplication.

use dashmap::DashSet;
use gravityfile_core::InodeInfo;

/// Tracks seen inodes to prevent double-counting hardlinks.
///
/// When a file has multiple hardlinks, we only want to count its size once.
/// This tracker uses a concurrent set to track (inode, device) pairs.
#[derive(Debug, Default)]
pub struct InodeTracker {
    seen: DashSet<InodeInfo>,
}

impl InodeTracker {
    /// Create a new inode tracker.
    pub fn new() -> Self {
        Self {
            seen: DashSet::new(),
        }
    }

    /// Track an inode. Returns `true` if this is the first time seeing it.
    ///
    /// If the inode was already tracked, returns `false` indicating this
    /// is a hardlink to an already-counted file.
    pub fn track(&self, info: InodeInfo) -> bool {
        self.seen.insert(info)
    }

    /// Check if an inode has been seen (without tracking).
    pub fn has_seen(&self, info: &InodeInfo) -> bool {
        self.seen.contains(info)
    }

    /// Get the number of unique inodes tracked.
    pub fn len(&self) -> usize {
        self.seen.len()
    }

    /// Check if no inodes have been tracked.
    pub fn is_empty(&self) -> bool {
        self.seen.is_empty()
    }

    /// Clear all tracked inodes.
    pub fn clear(&self) {
        self.seen.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_track_new_inode() {
        let tracker = InodeTracker::new();
        let info = InodeInfo::new(12345, 1);

        assert!(tracker.track(info));
        assert!(!tracker.track(info)); // Second time returns false
    }

    #[test]
    fn test_has_seen() {
        let tracker = InodeTracker::new();
        let info = InodeInfo::new(12345, 1);

        assert!(!tracker.has_seen(&info));
        tracker.track(info);
        assert!(tracker.has_seen(&info));
    }

    #[test]
    fn test_different_devices() {
        let tracker = InodeTracker::new();
        let info1 = InodeInfo::new(12345, 1);
        let info2 = InodeInfo::new(12345, 2); // Same inode, different device

        assert!(tracker.track(info1));
        assert!(tracker.track(info2)); // Different device, so it's new
    }
}
