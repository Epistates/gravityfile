//! Startup-only graphics probing. No reader survives the query deadline.
use ratatui_image::picker::Picker;

#[cfg(unix)]
pub(crate) fn query_picker() -> std::io::Result<Picker> {
    use ratatui_image::picker::ProtocolType;
    use ratatui_image::picker::cap_parser::{Parser, QueryStdioOptions, Response};
    use std::io::Write;
    let is_tmux = std::env::var_os("TMUX").is_some();
    let query = Parser::query(is_tmux, QueryStdioOptions::default());
    let mut output = std::io::stdout().lock();
    output.write_all(query.as_bytes())?;
    output.flush()?;
    drop(output);
    let responses = read_responses(libc::STDIN_FILENO, std::time::Duration::from_millis(250))?;
    let mut font = (10, 20);
    let mut protocol = None;
    for response in responses {
        match response {
            Response::CellSize(Some((width, height))) if width > 0 && height > 0 => {
                font = (width, height)
            }
            Response::Kitty => protocol = Some(ProtocolType::Kitty),
            Response::Sixel if protocol.is_none() => protocol = Some(ProtocolType::Sixel),
            _ => {}
        }
    }
    // This constructor preserves environment/tmux handling without spawning a
    // stdio query thread. It is the upstream API for an already known font size.
    #[allow(deprecated)]
    let mut picker = Picker::from_fontsize(font.into());
    if !matches!(picker.protocol_type(), ProtocolType::Iterm2)
        && let Some(protocol) = protocol
    {
        picker.set_protocol_type(protocol);
    }
    Ok(picker)
}

// Console probing needs a separately qualified cancellable reader on Windows.
// Halfblocks retain image previews without starting a detached stdin reader.
#[cfg(not(unix))]
pub(crate) fn query_picker() -> std::io::Result<Picker> {
    Ok(Picker::halfblocks())
}

#[cfg(unix)]
fn read_responses(
    fd: std::os::fd::RawFd,
    timeout: std::time::Duration,
) -> std::io::Result<Vec<ratatui_image::picker::cap_parser::Response>> {
    use ratatui_image::picker::cap_parser::{Parser, Response};
    use std::io::{Error, ErrorKind};
    let started = std::time::Instant::now();
    let mut parser = Parser::new();
    let mut responses = Vec::new();
    // Bound malformed/incomplete terminal replies as well as elapsed time.
    for _ in 0..8192 {
        loop {
            let remaining = timeout.checked_sub(started.elapsed()).ok_or_else(|| {
                Error::new(ErrorKind::TimedOut, "terminal graphics query timed out")
            })?;
            let mut descriptor = libc::pollfd {
                fd,
                events: libc::POLLIN,
                revents: 0,
            };
            // SAFETY: poll receives one initialized descriptor for this call.
            let result = unsafe {
                libc::poll(
                    &mut descriptor,
                    1,
                    remaining.as_millis().clamp(1, i32::MAX as u128) as i32,
                )
            };
            if result < 0 {
                let error = Error::last_os_error();
                if error.kind() == ErrorKind::Interrupted {
                    continue;
                }
                return Err(error);
            }
            if result == 0 {
                continue;
            }
            if descriptor.revents & libc::POLLIN == 0 {
                return Err(Error::new(
                    ErrorKind::UnexpectedEof,
                    "terminal query input closed",
                ));
            }
            break;
        }
        let mut byte = 0u8;
        // SAFETY: startup owns stdin exclusively; poll established readability.
        // Reading one byte prevents consuming keystrokes following a status reply.
        let count = unsafe { libc::read(fd, (&mut byte as *mut u8).cast(), 1) };
        if count != 1 {
            return Err(if count < 0 {
                Error::last_os_error()
            } else {
                Error::new(ErrorKind::UnexpectedEof, "terminal query input closed")
            });
        }
        for response in parser.push(char::from(byte)) {
            if response == Response::Status {
                return Ok(responses);
            }
            responses.push(response);
        }
    }
    Err(Error::new(
        ErrorKind::InvalidData,
        "terminal graphics reply exceeds 8192 bytes",
    ))
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::{
        io::{Read, Write},
        os::{fd::AsRawFd, unix::net::UnixStream},
        time::Duration,
    };
    #[test]
    fn silent_query_times_out_without_a_reader_consuming_later_input() {
        let (mut input, mut peer) = UnixStream::pair().unwrap();
        assert_eq!(
            read_responses(input.as_raw_fd(), Duration::from_millis(10))
                .unwrap_err()
                .kind(),
            std::io::ErrorKind::TimedOut
        );
        peer.write_all(b":q").unwrap();
        let mut bytes = [0; 2];
        input.read_exact(&mut bytes).unwrap();
        assert_eq!(&bytes, b":q");
    }
    #[test]
    fn query_stops_at_status_and_preserves_trailing_keys() {
        let (mut input, mut peer) = UnixStream::pair().unwrap();
        peer.write_all(b"\x1b[6;20;10t\x1b[0nq").unwrap();
        let responses = read_responses(input.as_raw_fd(), Duration::from_secs(1)).unwrap();
        assert!(
            responses.contains(&ratatui_image::picker::cap_parser::Response::CellSize(
                Some((10, 20))
            ))
        );
        let mut key = [0];
        input.read_exact(&mut key).unwrap();
        assert_eq!(&key, b"q");
    }
}
