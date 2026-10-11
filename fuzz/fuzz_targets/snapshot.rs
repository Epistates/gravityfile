//! Saved scans come from users and other tools: any bytes must either load
//! or fail with an error, and whatever loads must survive a trip through
//! the ncdu writer unchanged in total.
#![no_main]

use gravityfile_scan::ncdu;
use gravityfile_scan::snapshot::{ImportOptions, read_from};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let options = ImportOptions {
        apparent: data.first().is_some_and(|byte| byte & 1 == 1),
        count_hard_links: false,
        max_entries: Some(10_000),
    };
    let Ok(tree) = read_from(data, &options) else {
        return;
    };
    let mut dump = Vec::new();
    ncdu::write(&tree, &mut dump).expect("writing to memory succeeds");
    let again = read_from(dump.as_slice(), &options).expect("our own dump reads back");
    assert_eq!(again.total_size(), tree.total_size());
    assert_eq!(again.total_files(), tree.total_files());
    assert_eq!(again.total_dirs(), tree.total_dirs());
});
