//! Application constants.

/// Duplicate groups kept, those wasting the most space first; the totals
/// still cover every group.
pub const MAX_DUPLICATE_GROUPS: usize = 10_000;

/// Minimum file size for duplicate detection (1KB).
pub const MIN_DUPLICATE_SIZE: u64 = 1024;

/// Channel buffer size for scan results.
pub const SCAN_CHANNEL_SIZE: usize = 100;

/// Channel buffer size for analysis results.
pub const ANALYSIS_CHANNEL_SIZE: usize = 10;

/// Event loop tick interval in milliseconds.
pub const TICK_INTERVAL_MS: u64 = 50;

/// Rows the file preview moves per scroll key or wheel step.
pub const PREVIEW_SCROLL_STEP: isize = 5;
