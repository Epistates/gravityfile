//! Supervised plugin processes. This is a crash/timeout boundary, not OS confinement.

use std::collections::HashMap;
use std::io;
#[cfg(test)]
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
    mpsc,
};
use std::time::Duration;

use tokio_util::sync::CancellationToken;

use crate::runtime::{BoxFuture, HOOK_NAMES};
use crate::{
    Hook, HookContext, HookResult, IsolatedContext, PluginConfig, PluginError, PluginHandle,
    PluginMetadata, PluginResult, PluginRuntime, SandboxConfig, Value,
};

mod protocol;
use protocol::protocol_error as protocol;
use protocol::{
    Failure, Reply, Request, Response, WorkerLog, decode, decode_reply, encode, read_frame,
    write_frame,
};
#[cfg(test)]
use protocol::{MAX_LOG_BYTES, WireValue};

const STARTUP_TIMEOUT_MS: u64 = 10_000;
const MAX_WORKERS: usize = 32;
static ACTIVE_WORKERS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

struct WorkerPermit;
impl WorkerPermit {
    fn acquire() -> PluginResult<Self> {
        // A compare-exchange loop rather than `fetch_update`, which newer Rust
        // deprecates in favour of `try_update`, unavailable at our MSRV.
        let mut count = ACTIVE_WORKERS.load(Ordering::Acquire);
        loop {
            if count >= MAX_WORKERS {
                return Err(PluginError::ConfigError {
                    message: "maximum of 32 concurrent plugin workers reached".into(),
                });
            }
            match ACTIVE_WORKERS.compare_exchange_weak(
                count,
                count + 1,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => return Ok(Self),
                Err(actual) => count = actual,
            }
        }
    }
}
impl Drop for WorkerPermit {
    fn drop(&mut self) {
        ACTIVE_WORKERS.fetch_sub(1, Ordering::AcqRel);
    }
}

struct Job {
    bytes: Vec<u8>,
    reply: mpsc::SyncSender<io::Result<Vec<u8>>>,
}
struct Worker {
    permit: Mutex<Option<WorkerPermit>>,
    child: Mutex<Child>,
    requests: mpsc::SyncSender<Job>,
    busy: AtomicBool,
    alive: AtomicBool,
}

fn terminate_child(child: &mut Child) {
    #[cfg(unix)]
    {
        // SAFETY: the unreaped child owns the process-group ID established at
        // spawn. Kill the group before wait() can release that PID for reuse.
        unsafe {
            libc::kill(-(child.id() as i32), libc::SIGKILL);
        }
    }
    let _ = child.kill();
    let _ = child.wait();
}

/// A spawned process is owned even before its pipes/I/O thread are ready.
struct StartingWorker(Option<Child>);
impl Drop for StartingWorker {
    fn drop(&mut self) {
        if let Some(child) = &mut self.0 {
            terminate_child(child);
        }
    }
}

type IoTask = Box<dyn FnOnce() + Send + 'static>;

impl Worker {
    fn spawn(executable: &Path, language: &str) -> PluginResult<Arc<Self>> {
        Self::spawn_with_io_thread(executable, language, |task| {
            std::thread::Builder::new()
                .name("gravityfile-plugin-ipc".into())
                .spawn(task)
                .map(|_| ())
        })
    }

    fn spawn_with_io_thread(
        executable: &Path,
        language: &str,
        start_thread: impl FnOnce(IoTask) -> io::Result<()>,
    ) -> PluginResult<Arc<Self>> {
        let mut command = Command::new(executable);
        command.args(["__plugin-worker", language]);
        Self::spawn_command(command, start_thread)
    }

    fn spawn_command(
        mut command: Command,
        start_thread: impl FnOnce(IoTask) -> io::Result<()>,
    ) -> PluginResult<Arc<Self>> {
        let permit = WorkerPermit::acquire()?;
        command
            .env_clear()
            .current_dir(std::env::temp_dir())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            command.process_group(0);
        }
        let mut starting = StartingWorker(Some(command.spawn()?));
        let child = starting.0.as_mut().expect("newly spawned worker");
        let mut input = child
            .stdin
            .take()
            .ok_or_else(|| protocol("worker stdin unavailable"))?;
        let mut output = child
            .stdout
            .take()
            .ok_or_else(|| protocol("worker stdout unavailable"))?;
        let (requests, jobs) = mpsc::sync_channel::<Job>(1);
        // Pipe I/O runs off the calling thread. A blocked write/read is interrupted
        // by killing the worker on deadline; payload allocation is frame-bounded.
        start_thread(Box::new(move || {
            while let Ok(job) = jobs.recv() {
                let result =
                    write_frame(&mut input, &job.bytes).and_then(|_| read_frame(&mut output));
                let failed = result.is_err();
                let _ = job.reply.send(result);
                if failed {
                    break;
                }
            }
        }))?;
        Ok(Arc::new(Self {
            permit: Mutex::new(Some(permit)),
            child: Mutex::new(starting.0.take().expect("worker startup ownership")),
            requests,
            busy: AtomicBool::new(false),
            alive: AtomicBool::new(true),
        }))
    }

    fn terminate(&self) {
        if !self.alive.swap(false, Ordering::AcqRel) {
            return;
        }
        let mut child = self.child.lock().unwrap_or_else(|error| error.into_inner());
        terminate_child(&mut child);
        self.permit
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .take();
    }

    fn request(self: &Arc<Self>, request: Request, timeout_ms: u64) -> PluginResult<Response> {
        let _lease = RequestLease::acquire(self, false)?;
        self.request_reserved(request, timeout_ms)
    }

    async fn request_async(
        self: &Arc<Self>,
        request: Request,
        timeout_ms: u64,
    ) -> PluginResult<Response> {
        self.request_async_with_dispatch(request, timeout_ms, |worker, request, timeout_ms| {
            worker.request_reserved(request, timeout_ms)
        })
        .await
    }

    async fn request_async_with_dispatch(
        self: &Arc<Self>,
        request: Request,
        timeout_ms: u64,
        dispatch: impl FnOnce(Arc<Self>, Request, u64) -> PluginResult<Response> + Send + 'static,
    ) -> PluginResult<Response> {
        let mut lease = RequestLease::acquire(self, true)?;
        let worker = Arc::clone(self);
        // Start the deadline before blocking-task admission: a saturated executor
        // must not postpone the configured timeout indefinitely. On timeout the
        // lease retires the worker. The task guard aborts work still queued; tasks
        // already running observe termination through the worker and its pipes.
        let deadline = tokio::time::Instant::now() + Duration::from_millis(timeout_ms);
        let task =
            tokio_util::task::AbortOnDropHandle::new(tokio::task::spawn_blocking(move || {
                dispatch(worker, request, timeout_ms)
            }));
        let result = tokio::time::timeout_at(deadline, task)
            .await
            .map_err(|_| PluginError::Timeout {
                name: "worker".into(),
                timeout_ms,
            })?
            .map_err(|error| protocol(error.to_string()))?;
        lease.terminate_on_drop = false;
        result
    }

    fn request_reserved(&self, request: Request, timeout_ms: u64) -> PluginResult<Response> {
        self.request_reserved_with_encoding(request, timeout_ms, encode)
    }

    fn request_reserved_with_encoding(
        &self,
        request: Request,
        timeout_ms: u64,
        encode_request: impl FnOnce(Request) -> PluginResult<Vec<u8>>,
    ) -> PluginResult<Response> {
        let started = std::time::Instant::now();
        let budget = Duration::from_millis(timeout_ms);
        if !self.alive.load(Ordering::Acquire) {
            return Err(protocol("plugin worker terminated; reload the plugin"));
        }
        let expected = request.expected_response();
        let bytes = encode_request(request)?;
        // Encoding belongs to the same budget as pipe I/O. It cannot be
        // preempted here, but expired work must not receive a fresh reply budget.
        if started.elapsed() >= budget {
            self.terminate();
            return Err(PluginError::Timeout {
                name: "worker".into(),
                timeout_ms,
            });
        }
        let (reply, result) = mpsc::sync_channel(1);
        if let Err(error) = self.requests.try_send(Job { bytes, reply }) {
            // No request can be serviced after disconnection. A full channel
            // also violates exclusive admission; neither state is reusable.
            self.terminate();
            return Err(protocol(match error {
                mpsc::TrySendError::Disconnected(_) => "plugin worker channel closed",
                mpsc::TrySendError::Full(_) => "plugin worker channel unexpectedly full",
            }));
        }
        match result.recv_timeout(budget.saturating_sub(started.elapsed())) {
            Ok(Ok(bytes)) => match decode_reply(&bytes) {
                Ok(reply) => {
                    if let Ok(response) = &reply.result
                        && response.kind() != expected
                    {
                        self.terminate();
                        return Err(protocol(format!(
                            "unexpected plugin response: expected {expected:?}, received {:?}",
                            response.kind()
                        )));
                    }
                    if !reply.log.is_empty() {
                        tracing::info!(target: "plugin_worker", output = %reply.log, truncated = reply.log_truncated, "Plugin log");
                    }
                    match reply.result {
                        Ok(response) => Ok(response),
                        Err(error) => {
                            self.terminate();
                            if let Some(timeout_ms) = error.timeout_ms {
                                Err(PluginError::Timeout {
                                    name: "worker".into(),
                                    timeout_ms,
                                })
                            } else if error.cancelled {
                                Err(PluginError::Cancelled {
                                    name: "worker".into(),
                                })
                            } else {
                                Err(protocol(error.message))
                            }
                        }
                    }
                }
                Err(error) => {
                    self.terminate();
                    Err(error)
                }
            },
            Ok(Err(error)) => {
                self.terminate();
                Err(error.into())
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                self.terminate();
                Err(PluginError::Timeout {
                    name: "worker".into(),
                    timeout_ms,
                })
            }
            Err(error) => {
                self.terminate();
                Err(protocol(error.to_string()))
            }
        }
    }
}
/// Owns exclusive request admission independently of the blocking I/O task.
/// An abandoned async caller invalidates the worker's potentially changed state.
struct RequestLease {
    worker: Arc<Worker>,
    terminate_on_drop: bool,
}

impl RequestLease {
    fn acquire(worker: &Arc<Worker>, terminate_on_drop: bool) -> PluginResult<Self> {
        worker
            .busy
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .map_err(|_| protocol("plugin already has an active request"))?;
        Ok(Self {
            worker: Arc::clone(worker),
            terminate_on_drop,
        })
    }
}

impl Drop for RequestLease {
    fn drop(&mut self) {
        if self.terminate_on_drop {
            self.worker.terminate();
        }
        self.worker.busy.store(false, Ordering::Release);
    }
}

impl Drop for Worker {
    fn drop(&mut self) {
        self.terminate();
    }
}

struct Loaded {
    worker: Arc<Worker>,
    remote: usize,
    metadata: PluginMetadata,
    hooks: Vec<String>,
}

/// Persistent, independently supervised worker process for each loaded plugin.
/// The executable must implement the hidden `__plugin-worker LANGUAGE` entrypoint.
pub struct ProcessRuntime {
    executable: PathBuf,
    language: &'static str,
    config: PluginConfig,
    plugins: HashMap<PluginHandle, Loaded>,
    next: usize,
}
impl ProcessRuntime {
    /// Run Lua plugins in workers launched from `executable`, which must
    /// support the private worker protocol.
    pub fn new(executable: PathBuf) -> PluginResult<Self> {
        Ok(Self {
            executable: executable.canonicalize()?,
            language: "lua",
            config: PluginConfig::default(),
            plugins: HashMap::new(),
            next: 0,
        })
    }
    fn loaded(&self, handle: PluginHandle) -> PluginResult<&Loaded> {
        self.plugins
            .get(&handle)
            .ok_or_else(|| protocol("unknown plugin handle"))
    }
}

impl Drop for ProcessRuntime {
    fn drop(&mut self) {
        for plugin in self.plugins.values() {
            plugin.worker.terminate();
        }
    }
}

impl PluginRuntime for ProcessRuntime {
    fn name(&self) -> &'static str {
        self.language
    }
    fn file_extensions(&self) -> &'static [&'static str] {
        &[".lua"]
    }
    fn init(&mut self, config: &PluginConfig) -> PluginResult<()> {
        if config.default_timeout_ms == 0 {
            return Err(PluginError::ConfigError {
                message: "worker timeout must be nonzero".into(),
            });
        }
        self.config = config.clone();
        Ok(())
    }
    fn load_plugin(&mut self, id: &str, source: &Path) -> PluginResult<PluginHandle> {
        let source = source.canonicalize()?;
        let worker = Worker::spawn(&self.executable, self.language)?;
        worker.request(Request::Init(self.config.clone()), STARTUP_TIMEOUT_MS)?;
        let response = worker.request(
            Request::Load {
                name: id.into(),
                source,
            },
            self.config.default_timeout_ms,
        )?;
        let Response::Loaded {
            handle: remote,
            metadata,
            hooks,
        } = response
        else {
            return Err(protocol("unexpected load response"));
        };
        let handle = PluginHandle::new(self.next);
        self.next = self
            .next
            .checked_add(1)
            .ok_or_else(|| protocol("plugin handle space exhausted"))?;
        self.plugins.insert(
            handle,
            Loaded {
                worker,
                remote,
                metadata: *metadata,
                hooks,
            },
        );
        Ok(handle)
    }
    fn unload_plugin(&mut self, handle: PluginHandle) -> PluginResult<()> {
        let loaded = self
            .plugins
            .remove(&handle)
            .ok_or_else(|| protocol("unknown plugin handle"))?;
        loaded.worker.terminate();
        Ok(())
    }
    fn get_metadata(&self, handle: PluginHandle) -> Option<&PluginMetadata> {
        self.plugins.get(&handle).map(|p| &p.metadata)
    }
    fn has_hook(&self, handle: PluginHandle, name: &str) -> bool {
        self.plugins
            .get(&handle)
            .is_some_and(|p| p.hooks.iter().any(|hook| hook == name))
    }
    fn call_hook_sync(
        &self,
        handle: PluginHandle,
        hook: &Hook,
        ctx: &HookContext,
    ) -> PluginResult<HookResult> {
        let plugin = self.loaded(handle)?;
        let timeout = if hook.name() == "on_render" {
            self.config.default_timeout_ms.min(50)
        } else {
            self.config.default_timeout_ms
        };
        hook_response(plugin.worker.request(
            Request::Hook {
                handle: plugin.remote,
                hook: hook.clone(),
                context: ctx.into(),
            },
            timeout,
        )?)
    }
    fn call_hook_async<'a>(
        &'a self,
        handle: PluginHandle,
        hook: &'a Hook,
        ctx: &'a HookContext,
    ) -> BoxFuture<'a, PluginResult<HookResult>> {
        Box::pin(async move {
            let plugin = self.loaded(handle)?;
            let worker = Arc::clone(&plugin.worker);
            let request = Request::Hook {
                handle: plugin.remote,
                hook: hook.clone(),
                context: ctx.into(),
            };
            let timeout = self.config.default_timeout_ms;
            hook_response(worker.request_async(request, timeout).await?)
        })
    }
    fn call_method<'a>(
        &'a self,
        handle: PluginHandle,
        method: &'a str,
        args: Vec<Value>,
    ) -> BoxFuture<'a, PluginResult<Value>> {
        Box::pin(async move {
            let plugin = self.loaded(handle)?;
            let request = Request::Method {
                handle: plugin.remote,
                name: method.into(),
                args: args.into_iter().map(Into::into).collect(),
            };
            let worker = Arc::clone(&plugin.worker);
            let timeout = self.config.default_timeout_ms;
            value_response(worker.request_async(request, timeout).await?)
        })
    }
    fn create_isolated_context(
        &self,
        sandbox: &SandboxConfig,
    ) -> PluginResult<Box<dyn IsolatedContext>> {
        if sandbox.timeout_ms == 0 {
            return Err(PluginError::ConfigError {
                message: "worker timeout must be nonzero".into(),
            });
        }
        let worker = Worker::spawn(&self.executable, self.language)?;
        worker.request(Request::Isolate(sandbox.clone()), STARTUP_TIMEOUT_MS)?;
        Ok(Box::new(ProcessContext {
            worker,
            timeout: sandbox.timeout_ms,
        }))
    }
    fn loaded_plugins(&self) -> Vec<PluginHandle> {
        let mut handles: Vec<_> = self.plugins.keys().copied().collect();
        handles.sort_by_key(|h| h.id());
        handles
    }
    fn shutdown(&mut self) -> PluginResult<()> {
        for plugin in self.plugins.values() {
            plugin.worker.terminate();
        }
        self.plugins.clear();
        Ok(())
    }
}

fn hook_response(response: Response) -> PluginResult<HookResult> {
    match response {
        Response::Hook {
            handled,
            prevent_default,
            stop_propagation,
            value,
            error,
            notices,
        } => Ok(HookResult {
            handled,
            prevent_default,
            stop_propagation,
            value: value.map(Into::into),
            error,
            notices,
            plugin: None,
        }),
        _ => Err(protocol("unexpected hook response")),
    }
}
fn value_response(response: Response) -> PluginResult<Value> {
    match response {
        Response::Value(value) => Ok(value.into()),
        _ => Err(protocol("unexpected value response")),
    }
}

struct ProcessContext {
    worker: Arc<Worker>,
    timeout: u64,
}
impl Drop for ProcessContext {
    fn drop(&mut self) {
        self.worker.terminate();
    }
}
impl ProcessContext {
    async fn request(&self, request: Request, cancel: CancellationToken) -> PluginResult<Value> {
        let timeout = self.timeout;
        tokio::select! {
            biased;
            // Dropping request_async terminates only if it acquired the lease.
            // Cancellation before admission must not kill another active call.
            _ = cancel.cancelled() => Err(PluginError::Cancelled { name: "worker".into() }),
            result = self.worker.request_async(request, timeout) => value_response(result?),
        }
    }
}
impl IsolatedContext for ProcessContext {
    fn execute<'a>(
        &'a self,
        code: &'a [u8],
        cancel: CancellationToken,
    ) -> BoxFuture<'a, PluginResult<Value>> {
        Box::pin(async move {
            if code.len() > 1024 * 1024 {
                return Err(protocol("plugin source exceeds 1 MiB"));
            }
            self.request(Request::Execute(code.to_vec()), cancel).await
        })
    }
    fn call_function<'a>(
        &'a self,
        name: &'a str,
        args: Vec<Value>,
        cancel: CancellationToken,
    ) -> BoxFuture<'a, PluginResult<Value>> {
        Box::pin(self.request(
            Request::Function {
                name: name.into(),
                args: args.into_iter().map(Into::into).collect(),
            },
            cancel,
        ))
    }
    fn set_global(&mut self, name: &str, value: Value) -> PluginResult<()> {
        self.worker
            .request(
                Request::Set {
                    name: name.into(),
                    value: value.into(),
                },
                self.timeout,
            )
            .map(|_| ())
    }
    fn get_global(&self, name: &str) -> PluginResult<Value> {
        value_response(
            self.worker
                .request(Request::Get(name.into()), self.timeout)?,
        )
    }
}

/// Serve the private framed protocol. Call before initializing CLI logging or
/// printing anything to stdout. Not an authenticated external service.
pub fn run_worker(language: &str) -> PluginResult<()> {
    let log = WorkerLog::default();
    let output_log = log.clone();
    tracing_subscriber::fmt()
        .with_ansi(false)
        .with_writer(move || output_log.clone())
        .try_init()
        .map_err(|error| protocol(error.to_string()))?;
    let mut runtime: Box<dyn PluginRuntime> = match language {
        "lua" => Box::new(crate::lua::LuaRuntime::new()?),
        other => {
            return Err(PluginError::RuntimeNotAvailable {
                runtime: other.into(),
            });
        }
    };
    let executor = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    let mut isolated: Option<Box<dyn IsolatedContext>> = None;
    let mut input = io::stdin().lock();
    let mut output = io::stdout().lock();
    loop {
        let bytes = match read_frame(&mut input) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == io::ErrorKind::UnexpectedEof => break,
            Err(error) => return Err(error.into()),
        };
        let request: Request = decode(&bytes)?;
        let shutdown = matches!(request, Request::Shutdown);
        let result = execute_request(&mut *runtime, &mut isolated, &executor, request)
            .map_err(Failure::from);
        let (log_text, log_truncated) = log.take();
        let bytes = match encode(Reply {
            result,
            log: log_text,
            log_truncated,
        }) {
            Ok(bytes) => bytes,
            Err(error) => encode(Reply {
                result: Err(error.into()),
                log: String::new(),
                log_truncated: true,
            })?,
        };
        write_frame(&mut output, &bytes)?;
        if shutdown {
            break;
        }
    }
    executor.shutdown_background();
    Ok(())
}

fn execute_request(
    runtime: &mut dyn PluginRuntime,
    isolated: &mut Option<Box<dyn IsolatedContext>>,
    executor: &tokio::runtime::Runtime,
    request: Request,
) -> PluginResult<Response> {
    match request {
        Request::Init(config) => {
            runtime.init(&config)?;
            Ok(Response::Unit)
        }
        Request::Load { name, source } => {
            let handle = runtime.load_plugin(&name, &source)?;
            let metadata = runtime
                .get_metadata(handle)
                .ok_or_else(|| protocol("missing plugin metadata"))?
                .clone();
            let hooks = HOOK_NAMES
                .iter()
                .filter(|name| runtime.has_hook(handle, name))
                .map(|name| (*name).to_string())
                .collect();
            Ok(Response::Loaded {
                handle: handle.id(),
                metadata: Box::new(metadata),
                hooks,
            })
        }
        Request::Hook {
            handle,
            hook,
            context,
        } => {
            let result =
                runtime.call_hook_sync(PluginHandle::new(handle), &hook, &context.into())?;
            Ok(Response::Hook {
                handled: result.handled,
                prevent_default: result.prevent_default,
                stop_propagation: result.stop_propagation,
                value: result.value.map(Into::into),
                error: result.error,
                notices: result.notices,
            })
        }
        Request::Method { handle, name, args } => Ok(Response::Value(
            executor
                .block_on(runtime.call_method(
                    PluginHandle::new(handle),
                    &name,
                    args.into_iter().map(Into::into).collect(),
                ))?
                .into(),
        )),
        Request::Isolate(sandbox) => {
            *isolated = Some(runtime.create_isolated_context(&sandbox)?);
            Ok(Response::Unit)
        }
        Request::Shutdown => {
            runtime.shutdown()?;
            Ok(Response::Unit)
        }
        request => {
            let context = isolated
                .as_mut()
                .ok_or_else(|| protocol("isolated context is not initialized"))?;
            match request {
                Request::Execute(code) => Ok(Response::Value(
                    executor
                        .block_on(context.execute(&code, CancellationToken::new()))?
                        .into(),
                )),
                Request::Function { name, args } => Ok(Response::Value(
                    executor
                        .block_on(context.call_function(
                            &name,
                            args.into_iter().map(Into::into).collect(),
                            CancellationToken::new(),
                        ))?
                        .into(),
                )),
                Request::Set { name, value } => {
                    context.set_global(&name, value.into())?;
                    Ok(Response::Unit)
                }
                Request::Get(name) => Ok(Response::Value(context.get_global(&name)?.into())),
                _ => Err(protocol("invalid isolated request")),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn expired_encoding_budget_prevents_worker_dispatch() {
        let root = tempfile::tempdir().unwrap();
        let script = root.path().join("worker");
        std::fs::write(&script, b"#!/bin/sh\ncat \"$0.response\"\nexec sleep 10\n").unwrap();
        let bytes = encode(Reply {
            result: Ok(Response::Unit),
            log: String::new(),
            log_truncated: false,
        })
        .unwrap();
        let mut response = std::fs::File::create(root.path().join("worker.response")).unwrap();
        write_frame(&mut response, &bytes).unwrap();
        let worker = spawn_script(&script).unwrap();
        let result = worker.request_reserved_with_encoding(Request::Shutdown, 20, |request| {
            std::thread::sleep(Duration::from_millis(50));
            encode(request)
        });
        assert!(
            matches!(result, Err(PluginError::Timeout { timeout_ms: 20, .. })),
            "expired encoding returned error {:?}",
            result.err()
        );
        assert!(!worker.alive.load(Ordering::Acquire));
        assert!(worker.permit.lock().unwrap().is_none());
        assert!(worker.child.lock().unwrap().try_wait().unwrap().is_some());
    }

    #[cfg(unix)]
    fn script_command(script: &Path) -> Command {
        // Interpret the fixture rather than execing a freshly written inode,
        // which can encounter ETXTBSY during concurrent process creation.
        let mut command = Command::new("/bin/sh");
        command.arg(script).args(["__plugin-worker", "lua"]);
        command
    }

    #[cfg(unix)]
    fn spawn_script(script: &Path) -> PluginResult<Arc<Worker>> {
        Worker::spawn_command(script_command(script), |task| {
            std::thread::Builder::new().spawn(task).map(|_| ())
        })
    }

    #[cfg(unix)]
    #[test]
    fn startup_guard_closes_pipes_inherited_by_worker_descendants() {
        use std::io::BufRead;
        use std::os::unix::process::CommandExt;
        let mut child = Command::new("/bin/sh")
            .args(["-c", "sleep 10 & printf 'ready\\n'; wait"])
            .process_group(0)
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let stdout = child.stdout.take().unwrap();
        let starting = StartingWorker(Some(child));
        let (send, receive) = mpsc::channel();
        let reader = std::thread::spawn(move || {
            let mut reader = io::BufReader::new(stdout);
            let mut ready = String::new();
            reader.read_line(&mut ready).unwrap();
            send.send(ready).unwrap();
            let mut remaining = Vec::new();
            reader.read_to_end(&mut remaining).unwrap();
            let _ = send.send("eof".into());
        });
        assert_eq!(
            receive.recv_timeout(Duration::from_secs(2)).unwrap(),
            "ready\n"
        );
        // Killing just the shell would leave sleep holding stdout for 10s.
        drop(starting);
        assert_eq!(receive.recv_timeout(Duration::from_secs(2)).unwrap(), "eof");
        reader.join().unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn failed_io_thread_startup_reaps_the_spawned_worker() {
        let root = tempfile::tempdir().unwrap();
        let executable = root.path().join("worker");
        let pid_file = root.path().join("worker.pid");
        std::fs::write(
            &executable,
            b"#!/bin/sh\nprintf '%s' \"$$\" > \"$0.pid\"\nexec sleep 10\n",
        )
        .unwrap();
        let mut pid = None;
        let error = Worker::spawn_command(script_command(&executable), |_task| {
            let deadline = std::time::Instant::now() + Duration::from_secs(2);
            while std::time::Instant::now() < deadline {
                pid = std::fs::read_to_string(&pid_file)
                    .ok()
                    .and_then(|text| text.parse::<libc::pid_t>().ok());
                if pid.is_some() {
                    break;
                }
                std::thread::sleep(Duration::from_millis(5));
            }
            assert!(pid.is_some(), "worker did not announce startup");
            Err(io::Error::other("injected I/O thread startup failure"))
        })
        .err()
        .unwrap();
        assert!(
            error
                .to_string()
                .contains("injected I/O thread startup failure")
        );
        let mut status = 0;
        // SAFETY: pid identifies the child from this test; status is writable.
        assert_eq!(
            unsafe { libc::waitpid(pid.unwrap(), &mut status, libc::WNOHANG) },
            -1
        );
        assert_eq!(
            io::Error::last_os_error().raw_os_error(),
            Some(libc::ECHILD)
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn cancellation_terminates_the_owned_request_but_not_another_call() {
        use std::future::Future;
        use std::task::Poll;
        let root = tempfile::tempdir().unwrap();
        let executable = root.path().join("worker");
        std::fs::write(&executable, b"#!/bin/sh\nexec sleep 10\n").unwrap();
        let worker = spawn_script(&executable).unwrap();
        let context = ProcessContext {
            worker: Arc::clone(&worker),
            timeout: 5000,
        };
        let active_cancel = CancellationToken::new();
        let mut active =
            Box::pin(context.request(Request::Execute(Vec::new()), active_cancel.clone()));
        std::future::poll_fn(|cx| {
            assert!(active.as_mut().poll(cx).is_pending());
            Poll::Ready(())
        })
        .await;
        let other_cancel = CancellationToken::new();
        other_cancel.cancel();
        assert!(matches!(
            context
                .request(Request::Execute(Vec::new()), other_cancel)
                .await,
            Err(PluginError::Cancelled { .. })
        ));
        assert!(worker.alive.load(Ordering::Acquire));
        assert!(worker.busy.load(Ordering::Acquire));
        active_cancel.cancel();
        assert!(matches!(active.await, Err(PluginError::Cancelled { .. })));
        assert!(!worker.alive.load(Ordering::Acquire));
        assert!(!worker.busy.load(Ordering::Acquire));
        assert!(worker.child.lock().unwrap().try_wait().unwrap().is_some());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn dropping_async_request_terminates_only_its_owned_worker_request() {
        use std::future::Future;
        use std::task::Poll;
        let root = tempfile::tempdir().unwrap();
        let executable = root.path().join("worker");
        std::fs::write(&executable, b"#!/bin/sh\nexec sleep 10\n").unwrap();
        let worker = spawn_script(&executable).unwrap();
        let mut first = Box::pin(worker.request_async(Request::Shutdown, 5000));
        std::future::poll_fn(|cx| {
            assert!(first.as_mut().poll(cx).is_pending());
            Poll::Ready(())
        })
        .await;
        assert!(worker.busy.load(Ordering::Acquire));
        // A rejected concurrent caller never owns the lease and cannot kill it.
        let error = worker
            .request_async(Request::Shutdown, 5000)
            .await
            .err()
            .unwrap();
        assert!(error.to_string().contains("active request"));
        assert!(worker.alive.load(Ordering::Acquire));
        drop(first);
        assert!(!worker.alive.load(Ordering::Acquire));
        assert!(!worker.busy.load(Ordering::Acquire));
        assert!(worker.child.lock().unwrap().try_wait().unwrap().is_some());
        let error = worker.request(Request::Shutdown, 5000).err().unwrap();
        assert!(error.to_string().contains("worker terminated"));
    }

    #[cfg(unix)]
    #[test]
    fn async_deadline_includes_blocking_executor_queue_time() {
        for abandon in [false, true] {
            let root = tempfile::tempdir().unwrap();
            let executable = root.path().join("worker");
            std::fs::write(&executable, b"#!/bin/sh\nexec sleep 30\n").unwrap();
            let worker = spawn_script(&executable).unwrap();
            let executor = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .max_blocking_threads(1)
                .build()
                .unwrap();
            let (release, wait) = mpsc::channel::<()>();
            let (ready, started) = mpsc::channel();
            let blocker = executor.spawn_blocking(move || {
                ready.send(()).unwrap();
                let _ = wait.recv();
            });
            started.recv_timeout(Duration::from_secs(5)).unwrap();
            let dispatched = Arc::new(AtomicBool::new(false));
            let observed = Arc::clone(&dispatched);
            executor.block_on(async {
                let future = worker.request_async_with_dispatch(
                    Request::Shutdown,
                    25,
                    move |worker, request, timeout| {
                        observed.store(true, Ordering::Release);
                        worker.request_reserved(request, timeout)
                    },
                );
                if abandon {
                    use std::future::Future;
                    let mut future = Box::pin(future);
                    std::future::poll_fn(|cx| {
                        assert!(future.as_mut().poll(cx).is_pending());
                        std::task::Poll::Ready(())
                    })
                    .await;
                    drop(future);
                } else {
                    let result = tokio::time::timeout(Duration::from_secs(2), future)
                        .await
                        .expect("worker deadline must expire while blocking executor is occupied");
                    assert!(matches!(
                        result,
                        Err(PluginError::Timeout { timeout_ms: 25, .. })
                    ));
                }
                assert!(!worker.alive.load(Ordering::Acquire));
                assert!(worker.permit.lock().unwrap().is_none());
                assert!(worker.child.lock().unwrap().try_wait().unwrap().is_some());
            });
            drop(release);
            executor.block_on(blocker).unwrap();
            // Runtime destruction waits for queued work, proving it can drain safely
            // after the deadline has already retired the worker.
            drop(executor);
            assert!(!worker.busy.load(Ordering::Acquire));
            assert!(
                !dispatched.load(Ordering::Acquire),
                "expired queued work must never dispatch"
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn disconnected_io_channel_terminates_worker_and_releases_permit() {
        let root = tempfile::tempdir().unwrap();
        let executable = root.path().join("worker");
        std::fs::write(&executable, b"#!/bin/sh\nexec sleep 30\n").unwrap();
        // Deterministically model an I/O thread exiting before the first request.
        // Keep the worker handle alive to prove cleanup does not depend on Drop.
        let worker = Worker::spawn_command(script_command(&executable), |task| {
            drop(task);
            Ok(())
        })
        .unwrap();
        let error = worker.request(Request::Shutdown, 2000).err().unwrap();
        assert!(error.to_string().contains("channel closed"), "{error:?}");
        assert!(!worker.alive.load(Ordering::Acquire));
        assert!(worker.permit.lock().unwrap().is_none());
        assert!(worker.child.lock().unwrap().try_wait().unwrap().is_some());
        assert!(!worker.busy.load(Ordering::Acquire));
        let error = worker.request(Request::Shutdown, 2000).err().unwrap();
        assert!(error.to_string().contains("worker terminated"), "{error:?}");
    }

    #[cfg(unix)]
    #[test]
    fn mismatched_reply_types_terminate_workers_before_reuse() {
        let cases = [
            (
                Request::Init(PluginConfig::default()),
                Response::Value(WireValue::Null),
            ),
            (Request::Get("example".into()), Response::Unit),
            (
                Request::Load {
                    name: "example".into(),
                    source: "example.lua".into(),
                },
                Response::Unit,
            ),
            (
                Request::Hook {
                    handle: 0,
                    hook: Hook::OnStartup,
                    context: (&HookContext::new()).into(),
                },
                Response::Unit,
            ),
            (
                Request::Set {
                    name: "example".into(),
                    value: WireValue::Null,
                },
                Response::Value(WireValue::Null),
            ),
        ];
        for (request, response) in cases {
            let root = tempfile::tempdir().unwrap();
            let executable = root.path().join("worker");
            std::fs::write(
                &executable,
                b"#!/bin/sh\ncat \"$0.response\"\nexec sleep 10\n",
            )
            .unwrap();
            let bytes = encode(Reply {
                result: Ok(response),
                log: String::new(),
                log_truncated: false,
            })
            .unwrap();
            let mut file = std::fs::File::create(root.path().join("worker.response")).unwrap();
            write_frame(&mut file, &bytes).unwrap();
            let worker = spawn_script(&executable).unwrap();
            let error = worker.request(request, 2000).err().unwrap();
            assert!(
                error.to_string().contains("unexpected plugin response"),
                "expected response-kind rejection, received {error:?}"
            );
            assert!(!worker.alive.load(Ordering::Acquire));
            let error = worker.request(Request::Shutdown, 2000).err().unwrap();
            assert!(error.to_string().contains("worker terminated"));
        }
    }

    #[cfg(unix)]
    #[test]
    fn oversized_log_reply_terminates_worker_and_rejects_reuse() {
        let root = tempfile::tempdir().unwrap();
        let executable = root.path().join("worker");
        std::fs::write(
            &executable,
            b"#!/bin/sh\ncat \"$0.response\"\nexec sleep 10\n",
        )
        .unwrap();
        let bytes = encode(Reply {
            result: Ok(Response::Unit),
            log: "x".repeat(MAX_LOG_BYTES + 1),
            log_truncated: false,
        })
        .unwrap();
        let mut response = std::fs::File::create(root.path().join("worker.response")).unwrap();
        write_frame(&mut response, &bytes).unwrap();
        let worker = spawn_script(&executable).unwrap();
        let error = worker.request(Request::Shutdown, 2000).err().unwrap();
        assert!(error.to_string().contains("log exceeds 64 KiB"));
        assert!(!worker.alive.load(Ordering::Acquire));
        let error = worker.request(Request::Shutdown, 2000).err().unwrap();
        assert!(error.to_string().contains("worker terminated"));
    }
}
