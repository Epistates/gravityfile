//! Application constants.

/// Number of items to move when pressing Page Up/Down.
pub const PAGE_SIZE: usize = 10;

/// Maximum number of duplicate groups to analyze.
pub const MAX_DUPLICATE_GROUPS: usize = 100;

/// Minimum file size for duplicate detection (1KB).
pub const MIN_DUPLICATE_SIZE: u64 = 1024;

/// Channel buffer size for scan results.
pub const SCAN_CHANNEL_SIZE: usize = 100;

/// Channel buffer size for analysis results.
pub const ANALYSIS_CHANNEL_SIZE: usize = 10;

/// Event loop tick interval in milliseconds.
pub const TICK_INTERVAL_MS: u64 = 50;
