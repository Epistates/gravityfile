//! A single terminal reader with a joined shutdown for external-editor handoff.
use std::{
    io,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread::{self, JoinHandle},
    time::Duration,
};

use crossterm::event::Event;
use tokio::sync::mpsc;

pub(crate) struct EventSource {
    receiver: mpsc::Receiver<io::Result<Event>>,
    stop: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
}

impl EventSource {
    pub(crate) fn new() -> io::Result<Self> {
        Self::with_reader(|| {
            if crossterm::event::poll(Duration::from_millis(50))? {
                crossterm::event::read().map(Some)
            } else {
                Ok(None)
            }
        })
    }

    fn with_reader(
        mut read: impl FnMut() -> io::Result<Option<Event>> + Send + 'static,
    ) -> io::Result<Self> {
        let (sender, receiver) = mpsc::channel(64);
        let stop = Arc::new(AtomicBool::new(false));
        let stopping = stop.clone();
        let worker = thread::Builder::new()
            .name("gravityfile-input".into())
            .spawn(move || {
                while !stopping.load(Ordering::Acquire) {
                    let mut event = match read() {
                        Ok(None) => continue,
                        Ok(Some(event)) => Ok(event),
                        Err(error) => Err(error),
                    };
                    let failed = event.is_err();
                    loop {
                        if stopping.load(Ordering::Acquire) {
                            return;
                        }
                        match sender.try_send(event) {
                            Ok(()) => break,
                            Err(mpsc::error::TrySendError::Closed(_)) => return,
                            Err(mpsc::error::TrySendError::Full(pending)) => {
                                event = pending;
                                // Bounded backpressure must remain interruptible on drop.
                                thread::park_timeout(Duration::from_millis(1));
                            }
                        }
                    }
                    if failed {
                        return;
                    }
                }
            })?;
        Ok(Self {
            receiver,
            stop,
            worker: Some(worker),
        })
    }

    pub(crate) async fn next(&mut self) -> Option<io::Result<Event>> {
        self.receiver.recv().await
    }

    pub(crate) fn try_next(&mut self) -> Result<io::Result<Event>, mpsc::error::TryRecvError> {
        self.receiver.try_recv()
    }
}

impl Drop for EventSource {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(worker) = self.worker.take() {
            worker.thread().unpark();
            // The terminal must have no competing reader when an editor starts.
            let _ = worker.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;

    #[test]
    fn full_input_queue_can_stop_and_join_without_a_consumer() {
        let reads = Arc::new(AtomicUsize::new(0));
        let worker_reads = reads.clone();
        let source = EventSource::with_reader(move || {
            worker_reads.fetch_add(1, Ordering::SeqCst);
            Ok(Some(Event::Resize(80, 24)))
        })
        .unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while reads.load(Ordering::SeqCst) < 65 {
            assert!(std::time::Instant::now() < deadline);
            thread::yield_now();
        }
        drop(source);
        assert_eq!(reads.load(Ordering::SeqCst), 65);
    }

    #[tokio::test]
    async fn reader_error_is_delivered_before_stream_closure() {
        let mut source =
            EventSource::with_reader(|| Err(io::Error::other("reader failed"))).unwrap();
        assert_eq!(
            source.next().await.unwrap().unwrap_err().to_string(),
            "reader failed"
        );
        assert!(source.next().await.is_none());
    }
}
