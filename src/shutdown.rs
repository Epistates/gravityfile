//! Process signal handling for noninteractive observation commands.

use std::io;
use std::thread::JoinHandle;
use tokio_util::sync::CancellationToken;

pub(crate) struct Shutdown {
    cancel: CancellationToken,
    stop: CancellationToken,
    thread: Option<JoinHandle<()>>,
}

impl Shutdown {
    pub(crate) fn install() -> io::Result<Self> {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?;
        let entered = runtime.enter();
        #[cfg(unix)]
        let (mut interrupt, mut terminate) = (
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt())?,
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?,
        );
        #[cfg(windows)]
        let (mut interrupt, mut terminate) = (
            tokio::signal::windows::ctrl_c()?,
            tokio::signal::windows::ctrl_break()?,
        );
        drop(entered);
        let cancel = CancellationToken::new();
        let stop = CancellationToken::new();
        let work = cancel.clone();
        let done = stop.clone();
        let thread = std::thread::Builder::new()
            .name("gravityfile-signals".into())
            .spawn(move || {
                runtime.block_on(async move {
                    loop {
                        let code = tokio::select! {
                            biased;
                            _ = done.cancelled() => break,
                            signal = interrupt.recv() => { if signal.is_none() { break; } 130 },
                            signal = terminate.recv() => { if signal.is_none() { break; } 143 },
                        };
                        if work.is_cancelled() {
                            // A second signal deliberately skips graceful cleanup.
                            std::process::exit(code);
                        }
                        work.cancel();
                    }
                });
            })?;
        Ok(Self {
            cancel,
            stop,
            thread: Some(thread),
        })
    }

    pub(crate) fn token(&self) -> CancellationToken {
        self.cancel.clone()
    }
}

impl Drop for Shutdown {
    fn drop(&mut self) {
        self.stop.cancel();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader, Write};
    use std::process::{Command, Stdio};
    use std::time::{Duration, Instant};

    #[test]
    fn signal_child() {
        let Ok(mode) = std::env::var("GRAVITYFILE_SIGNAL_TEST") else {
            return;
        };
        let shutdown = Shutdown::install().unwrap();
        println!("READY");
        std::io::stdout().flush().unwrap();
        while !shutdown.token().is_cancelled() {
            std::thread::sleep(Duration::from_millis(1));
        }
        println!("CANCELLED");
        std::io::stdout().flush().unwrap();
        if mode == "force" {
            std::thread::sleep(Duration::from_secs(10));
            panic!("second signal did not terminate the process");
        }
    }

    #[test]
    fn signals_cancel_then_force_exit_and_listener_shuts_down() {
        for (signal, code) in [("-INT", 130), ("-TERM", 143)] {
            for mode in ["graceful", "force"] {
                let mut child = Command::new(std::env::current_exe().unwrap())
                    .args(["--exact", "shutdown::tests::signal_child", "--nocapture"])
                    .env("GRAVITYFILE_SIGNAL_TEST", mode)
                    .stdout(Stdio::piped())
                    .spawn()
                    .unwrap();
                let stdout = child.stdout.take().unwrap();
                let (send, receive) = std::sync::mpsc::channel();
                std::thread::spawn(move || {
                    for line in BufReader::new(stdout).lines() {
                        if send.send(line.unwrap()).is_err() {
                            break;
                        }
                    }
                });
                let wait_line = |expected: &str| {
                    while receive.recv_timeout(Duration::from_secs(5)).unwrap() != expected {}
                };
                wait_line("READY");
                assert!(
                    Command::new("kill")
                        .args([signal, &child.id().to_string()])
                        .status()
                        .unwrap()
                        .success()
                );
                wait_line("CANCELLED");
                if mode == "force" {
                    assert!(
                        Command::new("kill")
                            .args([signal, &child.id().to_string()])
                            .status()
                            .unwrap()
                            .success()
                    );
                }
                let deadline = Instant::now() + Duration::from_secs(5);
                let status = loop {
                    if let Some(status) = child.try_wait().unwrap() {
                        break status;
                    }
                    if Instant::now() >= deadline {
                        let _ = child.kill();
                        let _ = child.wait();
                        panic!("signal child failed to exit");
                    }
                    std::thread::sleep(Duration::from_millis(5));
                };
                assert_eq!(status.code(), Some(if mode == "force" { code } else { 0 }));
            }
        }
    }
}
