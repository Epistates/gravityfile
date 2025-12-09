# gravityfile-analyze

Analysis algorithms for gravityfile: duplicate detection, age analysis, and more.

This crate provides analysis capabilities that operate on scanned file trees to identify duplicates, stale directories, and other insights.

## Features

- **Duplicate Detection** - Find duplicate files using BLAKE3 hashing
- **Partial Hash Optimization** - Three-phase algorithm minimizes disk I/O
- **Age Analysis** - Categorize files by age and identify stale directories
- **Configurable** - Builder patterns for customizing analysis behavior
- **Parallel Processing** - Uses rayon for multi-threaded hash computation

## Duplicate Detection

### Basic Usage

```rust
use gravityfile_analyze::{DuplicateFinder, DuplicateConfig};
use gravityfile_scan::{JwalkScanner, ScanConfig};

// First, scan the directory
let scan_config = ScanConfig::new("/path/to/scan");
let scanner = JwalkScanner::new();
let tree = scanner.scan(&scan_config)?;

// Find duplicates with default settings
let finder = DuplicateFinder::new();
let report = finder.find_duplicates(&tree);

println!("Found {} duplicate groups", report.group_count);
println!("Wasted space: {} bytes", report.total_wasted_space);

for group in &report.groups {
    println!("\nDuplicate files ({} bytes each):", group.size);
    for path in &group.paths {
        println!("  {}", path.display());
    }
}
```

### Custom Configuration

```rust
use gravityfile_analyze::{DuplicateFinder, DuplicateConfig};

let config = DuplicateConfig::builder()
    .min_size(1024u64)              // Skip files < 1KB
    .max_size(1024 * 1024 * 1024)   // Skip files > 1GB
    .quick_compare(true)             // Use partial hash first
    .partial_hash_head(4096usize)   // Hash first 4KB
    .partial_hash_tail(4096usize)   // Hash last 4KB
    .exclude_patterns(vec![".git".into()])
    .max_groups(100usize)           // Limit results
    .build()?;

let finder = DuplicateFinder::with_config(config);
let report = finder.find_duplicates(&tree);
```

### Algorithm

The duplicate finder uses a three-phase algorithm for efficiency:

1. **Size Grouping** - Group files by size (O(n), instant)
2. **Partial Hash** - For size-matched files, compute hash of first + last 4KB
3. **Full Hash** - For partial-hash matches, compute full BLAKE3 hash

This approach minimizes disk I/O by eliminating non-duplicates early.

## Age Analysis

### Basic Usage

```rust
use gravityfile_analyze::{AgeAnalyzer, AgeConfig, format_age};

let analyzer = AgeAnalyzer::new();
let report = analyzer.analyze(&tree);

// Age distribution
for bucket in &report.buckets {
    println!("{}: {} files, {} bytes",
        bucket.name,
        bucket.file_count,
        bucket.total_size
    );
}

// Stale directories
println!("\nStale directories:");
for dir in &report.stale_directories {
    println!("  {} ({} bytes, {} old)",
        dir.path.display(),
        dir.size,
        format_age(dir.newest_file_age)
    );
}
```

### Custom Configuration

```rust
use gravityfile_analyze::{AgeAnalyzer, AgeConfig, AgeBucket};
use std::time::Duration;

let config = AgeConfig::builder()
    .stale_threshold(Duration::from_secs(180 * 24 * 60 * 60)) // 6 months
    .min_stale_size(10 * 1024 * 1024u64)  // 10MB minimum
    .max_stale_dirs(50usize)
    .buckets(vec![
        AgeBucket::new("Recent", Duration::from_secs(7 * 24 * 60 * 60)),
        AgeBucket::new("This Month", Duration::from_secs(30 * 24 * 60 * 60)),
        AgeBucket::new("This Quarter", Duration::from_secs(90 * 24 * 60 * 60)),
        AgeBucket::new("This Year", Duration::from_secs(365 * 24 * 60 * 60)),
        AgeBucket::new("Older", Duration::MAX),
    ])
    .build()?;

let analyzer = AgeAnalyzer::with_config(config);
let report = analyzer.analyze(&tree);
```

### Default Age Buckets

- **Today** - Modified within 24 hours
- **This Week** - Modified within 7 days
- **This Month** - Modified within 30 days
- **This Year** - Modified within 365 days
- **Older** - Everything else

## Types

### DuplicateReport

```rust
pub struct DuplicateReport {
    pub groups: Vec<DuplicateGroup>,      // Sorted by wasted space
    pub total_duplicate_size: u64,         // Size of all duplicates
    pub total_wasted_space: u64,           // Reclaimable space
    pub files_analyzed: u64,
    pub files_with_duplicates: u64,
    pub group_count: usize,
}
```

### DuplicateGroup

```rust
pub struct DuplicateGroup {
    pub hash: ContentHash,     // BLAKE3 hash
    pub size: u64,             // Size of each file
    pub paths: Vec<PathBuf>,   // All duplicate paths
    pub wasted_bytes: u64,     // size * (count - 1)
}
```

### AgeReport

```rust
pub struct AgeReport {
    pub buckets: Vec<AgeBucketStats>,
    pub stale_directories: Vec<StaleDirectory>,
    pub total_files: u64,
    pub total_size: u64,
    pub average_age: Duration,
    pub median_age_bucket: String,
}
```

## Performance

- Parallel hash computation using rayon
- Memory-mapped I/O for large files (>128KB)
- Partial hashing reduces disk reads by ~95% for non-duplicates
- Size grouping eliminates files that can't possibly be duplicates

## License

Licensed under either of Apache License, Version 2.0 or MIT license at your option.
