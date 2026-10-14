//! Best-effort restoration, including partially initialized terminal sessions.
use std::io::{self, Write};

use crossterm::{
    cursor::Show,
    event::DisableMouseCapture,
    execute,
    terminal::{LeaveAlternateScreen, disable_raw_mode},
};

/// Arm before initialization: even a partially successful setup needs cleanup.
pub(crate) struct TerminalSession {
    armed: bool,
}

impl TerminalSession {
    pub(crate) fn new() -> Self {
        Self { armed: true }
    }

    pub(crate) fn restore(&mut self) -> io::Result<()> {
        self.armed = false;
        restore_terminal()
    }
}

impl Drop for TerminalSession {
    fn drop(&mut self) {
        if self.armed {
            let _ = self.restore();
        }
    }
}

pub(crate) fn restore_terminal() -> io::Result<()> {
    restore_with(disable_raw_mode, &mut io::stdout())
}

fn restore_with(raw: impl FnOnce() -> io::Result<()>, output: &mut impl Write) -> io::Result<()> {
    // Do not short-circuit: output failure must not prevent raw-mode restoration,
    // and a failed step must not prevent the remaining escape sequences.
    let raw = raw();
    let mouse = execute!(output, DisableMouseCapture);
    let screen = execute!(output, LeaveAlternateScreen);
    let cursor = execute!(output, Show);
    raw.and(mouse).and(screen).and(cursor)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn raw_restore_failure_still_restores_screen_mouse_and_cursor() {
        let mut output = Vec::new();
        let error = restore_with(
            || Err(io::Error::other("raw restoration failed")),
            &mut output,
        )
        .unwrap_err();
        assert_eq!(error.to_string(), "raw restoration failed");
        let output = String::from_utf8(output).unwrap();
        // Windows changes mouse capture through the console API instead.
        #[cfg(unix)]
        assert!(output.contains("\x1b[?1000l"));
        assert!(output.contains("\x1b[?1049l"));
        assert!(output.contains("\x1b[?25h"));
    }

    #[test]
    fn output_failures_do_not_skip_raw_or_later_output_attempts() {
        struct BrokenOutput(usize);
        impl Write for BrokenOutput {
            fn write(&mut self, _: &[u8]) -> io::Result<usize> {
                self.0 += 1;
                Err(io::Error::new(io::ErrorKind::BrokenPipe, "closed"))
            }
            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }
        let mut raw_called = false;
        let mut output = BrokenOutput(0);
        let error = restore_with(
            || {
                raw_called = true;
                Ok(())
            },
            &mut output,
        )
        .unwrap_err();
        assert!(raw_called);
        // On Windows the mouse step uses the console API, not this output, and
        // its own failure is the one reported.
        if cfg!(windows) {
            assert_eq!(output.0, 2);
        } else {
            assert_eq!(error.kind(), io::ErrorKind::BrokenPipe);
            assert_eq!(output.0, 3);
        }
    }
}
