//! Manual end-to-end copy measurements; excluded from routine correctness tests.

use std::fs::{self, File};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::Path;
use std::time::Instant;

fn verify_equal(source: &Path, destination: &Path) {
    assert_eq!(
        fs::metadata(source).unwrap().len(),
        fs::metadata(destination).unwrap().len()
    );
    let mut source = File::open(source).unwrap();
    let mut destination = File::open(destination).unwrap();
    let mut left = vec![0; 128 * 1024];
    let mut right = vec![0; left.len()];
    loop {
        let count = source.read(&mut left).unwrap();
        if count == 0 {
            assert_eq!(destination.read(&mut right).unwrap(), 0);
            break;
        }
        destination.read_exact(&mut right[..count]).unwrap();
        assert_eq!(left[..count], right[..count]);
    }
}

#[cfg(unix)]
fn allocated(path: &Path) -> Option<u64> {
    use std::os::unix::fs::MetadataExt;
    Some(fs::metadata(path).unwrap().blocks() * 512)
}

#[cfg(not(unix))]
fn allocated(_: &Path) -> Option<u64> {
    None
}

#[test]
#[ignore = "manual release-mode filesystem benchmark"]
#[allow(clippy::assertions_on_constants)]
fn benchmark_staged_copy() {
    // This ignored test must compile in debug builds but reject manual execution
    // there. A compile-time assertion would break ordinary debug test builds.
    assert!(!cfg!(debug_assertions), "run this benchmark with --release");
    const LENGTH: u64 = 64 * 1024 * 1024 + 17;
    let fixture = tempfile::tempdir().unwrap();
    let mut samples = Vec::new();
    for sparse in [false, true] {
        let source = fixture.path().join(if sparse { "sparse" } else { "dense" });
        let mut file = File::create(&source).unwrap();
        if sparse {
            file.set_len(LENGTH).unwrap();
            file.write_all(b"head").unwrap();
            file.seek(SeekFrom::Start(LENGTH / 2)).unwrap();
            file.write_all(b"middle").unwrap();
        } else {
            // Nonzero varying bytes prevent the zero-chunk optimization and
            // make offsets observable during independent content verification.
            let buffer: Vec<u8> = (0..128 * 1024)
                .map(|index| (index % 251 + 1) as u8)
                .collect();
            let mut remaining = LENGTH;
            while remaining > 0 {
                let count = remaining.min(buffer.len() as u64) as usize;
                file.write_all(&buffer[..count]).unwrap();
                remaining -= count as u64;
            }
        }
        file.sync_all().unwrap();
        drop(file);
        let source_allocated = allocated(&source);
        for run in 0..3 {
            let destination = fixture.path().join("destination");
            let started = Instant::now();
            let bytes = crate::copy::staged_new_copy(
                &source,
                &destination,
                true,
                &tokio_util::sync::CancellationToken::new(),
            )
            .unwrap();
            let elapsed = started.elapsed();
            assert_eq!(bytes, LENGTH);
            verify_equal(&source, &destination);
            samples.push(serde_json::json!({
                "workload": if sparse { "sparse" } else { "dense" },
                "run": run, "logical_bytes": bytes,
                "elapsed_seconds": elapsed.as_secs_f64(),
                "source_allocated_bytes": source_allocated,
                "destination_allocated_bytes": allocated(&destination),
                "contents_verified": true,
            }));
            fs::remove_file(destination).unwrap();
        }
    }
    fixture.close().unwrap();
    println!(
        "COPY_BENCHMARK={}",
        serde_json::json!({
            "schema_version": 1, "os": std::env::consts::OS,
            "arch": std::env::consts::ARCH, "samples": samples,
            "fixture_removed": true,
            "method": "release-mode staged copy with timestamps; creation, byte verification and removal excluded; warm-cache possible; no destination durability sync or RSS measurement"
        })
    );
}
