//! Private versioned IPC types, bounded framing, and response log handling.
//! This is a local parent/worker protocol, not an authenticated network service.

use crate::{
    Hook, HookContext, PluginConfig, PluginError, PluginMetadata, PluginResult, SandboxConfig,
    Value,
};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::io::{self, Read, Write};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

// 2: hook responses carry notices.
const VERSION: u32 = 2;
const MAX_FRAME: usize = 4 * 1024 * 1024;
pub(super) const MAX_LOG_BYTES: usize = 64 * 1024;

// The public untagged Value JSON representation cannot round-trip bytes or NaN.
// The internal protocol explicitly tags variants and transports float bits.
#[derive(Serialize, Deserialize)]
pub(super) enum WireValue {
    Null,
    Bool(bool),
    Integer(i64),
    Float(u64),
    String(String),
    Bytes(Vec<u8>),
    Array(Vec<WireValue>),
    Object(HashMap<String, WireValue>),
}

impl From<Value> for WireValue {
    fn from(value: Value) -> Self {
        match value {
            Value::Null => Self::Null,
            Value::Bool(v) => Self::Bool(v),
            Value::Integer(v) => Self::Integer(v),
            Value::Float(v) => Self::Float(v.to_bits()),
            Value::String(v) => Self::String(v),
            Value::Bytes(v) => Self::Bytes(v),
            Value::Array(v) => Self::Array(v.into_iter().map(Into::into).collect()),
            Value::Object(v) => Self::Object(v.into_iter().map(|(k, v)| (k, v.into())).collect()),
        }
    }
}
impl From<WireValue> for Value {
    fn from(value: WireValue) -> Self {
        match value {
            WireValue::Null => Self::Null,
            WireValue::Bool(v) => Self::Bool(v),
            WireValue::Integer(v) => Self::Integer(v),
            WireValue::Float(v) => Self::Float(f64::from_bits(v)),
            WireValue::String(v) => Self::String(v),
            WireValue::Bytes(v) => Self::Bytes(v),
            WireValue::Array(v) => Self::Array(v.into_iter().map(Into::into).collect()),
            WireValue::Object(v) => {
                Self::Object(v.into_iter().map(|(k, v)| (k, v.into())).collect())
            }
        }
    }
}

#[derive(Serialize, Deserialize)]
pub(super) struct Context {
    data: HashMap<String, WireValue>,
    cwd: Option<PathBuf>,
    view_root: Option<PathBuf>,
    theme: Option<String>,
}
impl From<&HookContext> for Context {
    fn from(ctx: &HookContext) -> Self {
        Self {
            data: ctx
                .data
                .iter()
                .map(|(k, v)| (k.clone(), v.clone().into()))
                .collect(),
            cwd: ctx.cwd.clone(),
            view_root: ctx.view_root.clone(),
            theme: ctx.theme.clone(),
        }
    }
}
impl From<Context> for HookContext {
    fn from(ctx: Context) -> Self {
        Self {
            data: ctx.data.into_iter().map(|(k, v)| (k, v.into())).collect(),
            cwd: ctx.cwd,
            view_root: ctx.view_root,
            theme: ctx.theme,
        }
    }
}

#[derive(Serialize, Deserialize)]
pub(super) enum Request {
    Init(PluginConfig),
    Load {
        name: String,
        source: PathBuf,
    },
    Hook {
        handle: usize,
        hook: Hook,
        context: Context,
    },
    Method {
        handle: usize,
        name: String,
        args: Vec<WireValue>,
    },
    Isolate(SandboxConfig),
    Execute(Vec<u8>),
    Function {
        name: String,
        args: Vec<WireValue>,
    },
    Set {
        name: String,
        value: WireValue,
    },
    Get(String),
    Shutdown,
}

#[derive(Serialize, Deserialize)]
pub(super) enum Response {
    Unit,
    Loaded {
        handle: usize,
        metadata: Box<PluginMetadata>,
        hooks: Vec<String>,
    },
    Hook {
        handled: bool,
        prevent_default: bool,
        stop_propagation: bool,
        value: Option<WireValue>,
        error: Option<String>,
        notices: Vec<crate::Notice>,
    },
    Value(WireValue),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ResponseKind {
    Unit,
    Loaded,
    Hook,
    Value,
}

impl Request {
    pub(super) fn expected_response(&self) -> ResponseKind {
        match self {
            Self::Init(_) | Self::Isolate(_) | Self::Set { .. } | Self::Shutdown => {
                ResponseKind::Unit
            }
            Self::Load { .. } => ResponseKind::Loaded,
            Self::Hook { .. } => ResponseKind::Hook,
            Self::Method { .. } | Self::Execute(_) | Self::Function { .. } | Self::Get(_) => {
                ResponseKind::Value
            }
        }
    }
}

impl Response {
    pub(super) fn kind(&self) -> ResponseKind {
        match self {
            Self::Unit => ResponseKind::Unit,
            Self::Loaded { .. } => ResponseKind::Loaded,
            Self::Hook { .. } => ResponseKind::Hook,
            Self::Value(_) => ResponseKind::Value,
        }
    }
}

#[derive(Serialize, Deserialize)]
pub(super) struct Failure {
    pub(super) message: String,
    pub(super) timeout_ms: Option<u64>,
    pub(super) cancelled: bool,
}
impl From<PluginError> for Failure {
    fn from(error: PluginError) -> Self {
        Self {
            message: error.to_string(),
            timeout_ms: match &error {
                PluginError::Timeout { timeout_ms, .. } => Some(*timeout_ms),
                _ => None,
            },
            cancelled: matches!(error, PluginError::Cancelled { .. }),
        }
    }
}
#[derive(Serialize, Deserialize)]
pub(super) struct Reply {
    pub(super) result: Result<Response, Failure>,
    pub(super) log: String,
    pub(super) log_truncated: bool,
}

#[derive(Clone, Default)]
pub(super) struct WorkerLog(Arc<Mutex<(Vec<u8>, bool)>>);
impl Write for WorkerLog {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let mut data = self.0.lock().unwrap_or_else(|error| error.into_inner());
        let available = MAX_LOG_BYTES.saturating_sub(data.0.len());
        data.0
            .extend_from_slice(&bytes[..bytes.len().min(available)]);
        data.1 |= bytes.len() > available;
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
impl WorkerLog {
    pub(super) fn take(&self) -> (String, bool) {
        let mut data = self.0.lock().unwrap_or_else(|error| error.into_inner());
        let (bytes, mut truncated) = std::mem::take(&mut *data);
        let mut message = String::from_utf8_lossy(&bytes).into_owned();
        // Replacement characters can expand invalid or cutoff UTF-8 bytes.
        // Apply the wire/logging budget to the final valid string as well.
        if message.len() > MAX_LOG_BYTES {
            let mut end = MAX_LOG_BYTES;
            while !message.is_char_boundary(end) {
                end -= 1;
            }
            message.truncate(end);
            truncated = true;
        }
        (message, truncated)
    }
}

#[derive(Serialize, Deserialize)]
struct Frame<T> {
    version: u32,
    body: T,
}

struct LimitedBuffer(Vec<u8>);
impl Write for LimitedBuffer {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if self.0.len().saturating_add(bytes.len()) > MAX_FRAME {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "plugin IPC frame exceeds 4 MiB",
            ));
        }
        self.0.extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
pub(super) fn encode<T: Serialize>(body: T) -> PluginResult<Vec<u8>> {
    let mut buffer = LimitedBuffer(Vec::new());
    serde_json::to_writer(
        &mut buffer,
        &Frame {
            version: VERSION,
            body,
        },
    )
    .map_err(|error| PluginError::Serialization(error.to_string()))?;
    Ok(buffer.0)
}
pub(super) fn decode<T: for<'de> Deserialize<'de>>(bytes: &[u8]) -> PluginResult<T> {
    let frame: Frame<T> = serde_json::from_slice(bytes)
        .map_err(|error| PluginError::Serialization(error.to_string()))?;
    if frame.version != VERSION {
        return Err(protocol_error("unsupported plugin IPC version"));
    }
    Ok(frame.body)
}

pub(super) fn decode_reply(bytes: &[u8]) -> PluginResult<Reply> {
    let reply: Reply = decode(bytes)?;
    if reply.log.len() > MAX_LOG_BYTES {
        return Err(protocol_error("plugin worker log exceeds 64 KiB"));
    }
    Ok(reply)
}
pub(super) fn write_frame(writer: &mut impl Write, bytes: &[u8]) -> io::Result<()> {
    writer.write_all(&(bytes.len() as u32).to_be_bytes())?;
    writer.write_all(bytes)?;
    writer.flush()
}
pub(super) fn read_frame(reader: &mut impl Read) -> io::Result<Vec<u8>> {
    let mut length = [0u8; 4];
    reader.read_exact(&mut length)?;
    let length = u32::from_be_bytes(length) as usize;
    if length > MAX_FRAME {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "oversized plugin IPC frame",
        ));
    }
    let mut bytes = vec![0; length];
    reader.read_exact(&mut bytes)?;
    Ok(bytes)
}

pub(super) fn protocol_error(message: impl Into<String>) -> PluginError {
    PluginError::ExecutionError {
        name: "worker".into(),
        message: message.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frames_reject_oversize_truncation_and_version_mismatch() {
        assert_eq!(
            read_frame(&mut &u32::MAX.to_be_bytes()[..])
                .unwrap_err()
                .kind(),
            io::ErrorKind::InvalidData
        );
        assert_eq!(
            read_frame(&mut &[0, 0, 0, 2, 1][..]).unwrap_err().kind(),
            io::ErrorKind::UnexpectedEof
        );
        // A frame from another protocol version is refused.
        let other = format!(r#"{{"version":{},"body":"Null"}}"#, VERSION + 1);
        assert!(decode::<WireValue>(other.as_bytes()).is_err());
        assert!(encode(WireValue::String("x".repeat(MAX_FRAME))).is_err());
    }

    #[test]
    fn wire_values_preserve_bytes_and_float_bits() {
        let bytes = WireValue::Bytes(vec![0, 255]);
        assert!(
            matches!(decode::<WireValue>(&encode(bytes).unwrap()).unwrap(), WireValue::Bytes(v) if v == [0, 255])
        );
        for bits in [
            f64::NAN.to_bits(),
            f64::INFINITY.to_bits(),
            (-0.0_f64).to_bits(),
        ] {
            let value: Value = decode::<WireValue>(&encode(WireValue::Float(bits)).unwrap())
                .unwrap()
                .into();
            assert!(matches!(value, Value::Float(v) if v.to_bits() == bits));
        }
    }

    #[test]
    fn worker_logs_are_bounded_and_drained() {
        let mut log = WorkerLog::default();
        log.write_all(&vec![b'x'; 100_000]).unwrap();
        let (message, truncated) = log.take();
        assert_eq!(message.len(), 64 * 1024);
        assert!(truncated);
        assert_eq!(log.take(), (String::new(), false));
    }

    #[test]
    fn worker_log_budget_applies_after_utf8_replacement() {
        for bytes in [
            vec![0xff; MAX_LOG_BYTES],
            "界".repeat(MAX_LOG_BYTES / 3 + 1).into_bytes(),
        ] {
            let mut log = WorkerLog::default();
            log.write_all(&bytes).unwrap();
            let (message, truncated) = log.take();
            assert!(message.len() <= MAX_LOG_BYTES);
            assert!(truncated);
            assert!(!message.is_empty());
            assert_eq!(log.take(), (String::new(), false));
            let frame = encode(Reply {
                result: Ok(Response::Unit),
                log: message,
                log_truncated: truncated,
            })
            .unwrap();
            assert!(decode_reply(&frame).is_ok());
        }
    }

    #[test]
    fn parent_rejects_oversized_worker_logs_even_within_frame_budget() {
        for truncated in [false, true] {
            for size in [MAX_LOG_BYTES, MAX_LOG_BYTES + 1] {
                let bytes = encode(Reply {
                    result: Ok(Response::Unit),
                    log: "x".repeat(size),
                    log_truncated: truncated,
                })
                .unwrap();
                assert!(bytes.len() < MAX_FRAME);
                assert_eq!(decode_reply(&bytes).is_ok(), size == MAX_LOG_BYTES);
            }
        }
    }
}
