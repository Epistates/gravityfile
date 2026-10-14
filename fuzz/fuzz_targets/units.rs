//! Size and age arguments: any string parses or fails cleanly, and
//! whatever parses also formats.
#![no_main]

use gravityfile_core::units::{Units, format_age, format_size, parse_age, parse_size};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|input: &str| {
    if let Ok(bytes) = parse_size(input) {
        format_size(bytes, Units::Si);
        format_size(bytes, Units::Binary);
    }
    if let Ok(age) = parse_age(input) {
        format_age(age);
    }
});
