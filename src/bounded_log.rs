//! Per-process diagnostic log bounds; this is not durable audit storage.
use std::io::{self, Write};
use std::sync::Mutex;

const MAX_LOG_BYTES: usize = 8 * 1024 * 1024;
const TRUNCATED: &[u8] = b"\n[gravityfile: log size limit reached; further diagnostics omitted]\n";

pub(crate) struct BoundedLog<W> {
    state: Mutex<State<W>>,
}

struct State<W> {
    writer: W,
    remaining: usize,
    stopped: bool,
}

impl<W: Write> BoundedLog<W> {
    pub(crate) fn new(writer: W) -> Self {
        Self::with_limit(writer, MAX_LOG_BYTES)
    }

    fn with_limit(writer: W, limit: usize) -> Self {
        assert!(limit >= TRUNCATED.len());
        Self {
            state: Mutex::new(State {
                writer,
                remaining: limit - TRUNCATED.len(),
                stopped: false,
            }),
        }
    }
}

// Arc<BoundedLog<File>> implements tracing's MakeWriter through this shared
// reference. One mutex serializes the byte budget across all logging threads.
impl<W: Write> Write for &BoundedLog<W> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        if state.stopped || bytes.is_empty() {
            return Ok(bytes.len());
        }
        let accepted = bytes.len().min(state.remaining);
        state.remaining -= accepted;
        let overflow = accepted < bytes.len();
        // Reserve budget before I/O. A partial write followed by an error must
        // never grant a retry another allocation from the same byte budget.
        let result = state.writer.write_all(&bytes[..accepted]);
        if let Err(error) = result {
            state.stopped = true;
            return Err(error);
        }
        if overflow {
            state.stopped = true;
            state.writer.write_all(TRUNCATED)?;
        }
        // Logging is best-effort after the cap; never make producers retry data
        // intentionally discarded from this diagnostic sink.
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        self.state
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .writer
            .flush()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    #[test]
    fn concurrent_file_writes_share_one_cap_and_one_truncation_marker() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("log");
        let log = Arc::new(BoundedLog::with_limit(
            std::fs::File::create(&path).unwrap(),
            1024,
        ));
        let threads: Vec<_> = (0..8)
            .map(|_| {
                let log = Arc::clone(&log);
                std::thread::spawn(move || {
                    for _ in 0..100 {
                        (&*log).write_all(&[b'x'; 37]).unwrap();
                    }
                })
            })
            .collect();
        for thread in threads {
            thread.join().unwrap();
        }
        (&*log).flush().unwrap();
        let bytes = std::fs::read(path).unwrap();
        assert_eq!(bytes.len(), 1024);
        assert!(bytes.ends_with(TRUNCATED));
        assert!(bytes[..1024 - TRUNCATED.len()].iter().all(|&b| b == b'x'));
    }

    #[test]
    fn below_limit_preserves_bytes_without_marker() {
        let log = BoundedLog::with_limit(Vec::new(), 128);
        (&log).write_all(b"first\nsecond\n").unwrap();
        (&log).write_all(b"").unwrap();
        assert_eq!(log.state.lock().unwrap().writer, b"first\nsecond\n");
    }

    #[test]
    fn partial_io_failure_disables_further_writes() {
        struct Failing {
            bytes: Vec<u8>,
            calls: usize,
        }
        impl Write for Failing {
            fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
                self.calls += 1;
                if self.calls > 1 {
                    return Err(io::Error::other("injected write failure"));
                }
                let n = bytes.len().min(3);
                self.bytes.extend_from_slice(&bytes[..n]);
                Ok(n)
            }
            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }
        let log = BoundedLog::with_limit(
            Failing {
                bytes: Vec::new(),
                calls: 0,
            },
            128,
        );
        assert!((&log).write_all(b"partial failure").is_err());
        (&log).write_all(b"must be discarded").unwrap();
        let state = log.state.lock().unwrap();
        assert_eq!(state.writer.bytes, b"par");
        assert_eq!(state.writer.calls, 2);
    }
}
