# Plugin process execution

The standard `grav` and `gravityfile` TUI launches a separate persistent child
process for every loaded Lua or Rhai plugin. Plugin globals and hook state remain
available across requests. Standalone isolated contexts each receive their own
worker. A failed worker does not invalidate other loaded plugins.

The parent enforces execution deadlines, including top-level plugin loading and
host calls that cannot be interrupted by a VM instruction hook. Worker startup
has a separate 10-second deadline. Execution uses the configured timeout;
synchronous render hooks have a maximum 50-millisecond budget. Async deadlines
start before blocking-task admission, so executor queue time counts toward the
configured timeout. Async process calls require a Tokio runtime with its time
driver enabled. Synchronous request serialization and reply waiting now share
one budget: if serialization exhausts it, the worker is retired before dispatch;
otherwise reply waiting receives only the remaining time. Serialization and
reply decoding are not preemptible on the calling thread, so this is not a hard
end-to-end latency guarantee. The [encoding-budget regression](qualification/plugin-encoding-budget.json)
injects encoding delay and verifies expiry even when a valid reply is ready.
Timeouts,
cancellation of an active isolate, malformed replies, and execution errors retire
the worker. Reload the plugin or create a fresh context after failure. Calls are
never automatically replayed: the failed invocation may already have side effects.

On Unix, termination kills the worker process group and reaps the worker. On
Windows, it kills and reaps the worker itself. Normal unload, context destruction,
and runtime shutdown also terminate workers. An abrupt host crash is not a
guarantee of immediate child termination during an active call.

A startup guard owns the child immediately after spawn, before pipe extraction
or I/O-thread creation. Setup failure uses the same termination path as normal
shutdown, including Unix process-group termination. Fault-injection coverage
checks a failed I/O-thread start, direct-child reaping, and EOF on a pipe inherited
by a worker descendant after guard cleanup. The latest verification is on macOS;
this does not establish Windows descendant termination or confinement against
processes that deliberately leave their Unix process group.

## Bounds and compatibility

- At most 32 active workers per host process; excess creation fails explicitly.
- Versioned, length-prefixed JSON messages have a 4 MiB encoded frame limit.
  Internal value tags preserve byte arrays and floating-point bit patterns.
- Plugin source files and standalone source inputs are limited to 1 MiB.
- Host-side `plugin.toml` reads are limited to 64 KiB before parsing. Both direct
  loading and discovery reject oversized/nonregular manifests; Unix reads also
  reject leaf symlinks and open without blocking on FIFOs. Discovery logs and
  skips an invalid manifest while continuing with other plugins.
- Worker tracing logs retain at most 64 KiB per response and report truncation.
  The limit also applies after UTF-8 replacement, which can expand a truncated
  multibyte sequence. The parent independently rejects oversized log text before
  forwarding it to tracing and terminates the worker through the protocol-error
  path. The 4 MiB frame cap still governs reading/decoding; the smaller log limit
  is not a bound on transient decoding or UTF-8 conversion allocations.
- Every successful reply is checked against its request type in the supervisor:
  initialization, isolation, assignment and shutdown require an acknowledgement;
  loading requires metadata; hooks require hook results; evaluation, methods,
  functions and global reads require values. A mismatch terminates the worker
  and prevents reuse, including for calls whose acknowledgement was previously
  ignored. This validates response shape, not the truth of plugin-provided data.
- Async worker calls reserve exclusive request ownership before dispatching
  blocking I/O. Dropping a polled call before its response is delivered terminates
  the worker, including when a caller uses an outer timeout or aborts its task.
  Its plugin must be reloaded, or its isolated context recreated. A concurrent
  call rejected as busy does not own that reservation and cannot terminate the
  active call merely by being dropped. An unpolled future has no such side
  effect. Explicit cancellation uses the same ownership guard: cancellation
  before admission does not terminate another call, while cancellation after
  acquiring the request terminates that worker. Worker termination/reaping is synchronous and has no hard OS latency
  guarantee. A task guard aborts blocking work that has not started when a call
  is dropped or times out. Already-running blocking tasks cannot be aborted by
  Tokio; worker termination interrupts their pipe I/O. Deadlines also depend on
  the async executor being polled and are not hard real-time guarantees.
  The [queued-task regression](qualification/plugin-queue-abort.json) occupies the
  sole blocking thread, then times out or drops the admitted call. After releasing
  the thread and draining the runtime, it verifies the queued dispatch never ran,
  the child was reaped, and its permit was released. The earlier deadline test
  proved timeout/reaping but did not detect detached queued work.
  Logs from a worker killed before its reply may be lost.
- A disconnected request channel, or an unexpectedly full channel after exclusive
  admission, invalidates the worker immediately. The host kills/reaps the child
  and releases its worker permit even when callers retain the runtime handle.
  The disconnected-channel regression injects I/O-thread exit before dispatch;
  it does not explain the earlier intermittent response-kind test failure.
- One request runs per worker; concurrent requests fail as busy.
- Workers start with an empty environment in the system temporary directory.
  Plugin filesystem paths should be absolute.

For an embedded TUI, set `TuiConfig::with_plugin_worker_executable` to an
absolute path to a compatible Gravityfile executable. Without this setting,
the embedded TUI does not register plugin runtimes. Library consumers can use
`ProcessRuntime` explicitly. Direct `LuaRuntime` and `RhaiRuntime` remain
available as deliberate in-process integrations with VM-level restrictions.

## Security boundary

This adds process crash containment and hard execution preemption. It does not
yet add OS-enforced filesystem/network capability confinement or physical memory
limits. Existing VM permissions and limits still apply, but are insufficient to
establish a sandbox for hostile native exploits. Unix process groups also do not
confine descendants that escape the group. Install trusted plugins only until
platform-specific confinement and adversarial qualification are complete.

Integration tests exercise real worker processes for both languages, persistent
state, runaway loading, cancellation, oversized results, and independent worker
survival. The [September 29 scoped qualification](qualification/plugin-cross-platform.json)
passed all six real-worker tests on macOS and Colima Linux, plus the 48 plugin
crate tests on Linux (one doctest ignored). Linux used Rust 1.90 with networking
disabled and read-only source. These results do not qualify native Windows.

## Manager lifecycle failures

Registering a runtime under an already registered name returns an error instead
of replacing the owner of existing plugin handles. An unload removes manager
metadata only after the runtime confirms success. Failed unload records remain
available for a cleanup retry.

Manager shutdown attempts every registered runtime even if an earlier shutdown
fails. It logs each failure, returns the first error, and removes records only
for runtimes that report successful shutdown. A failed runtime may already be
partially stopped: retained records support cleanup retries, not a guarantee that
its hooks remain callable. Runtime implementations should make cleanup retries
safe and report completion accurately.

Explicit manager loading honors the global plugin enable switch and per-plugin
disable list, just like discovery. A plugin name may have only one loaded
instance in a manager. Rediscovery skips loaded identities; explicit duplicate
loads return an error. Unload the existing plugin before reloading it. These
checks occur before runtime loading and worker creation.

Discovery enumerates candidate paths completely, then loads them in native path
sort order. Enumeration errors fail discovery before new plugins are loaded;
a missing plugin directory is still an empty discovery. Invalid individual
manifests continue to be logged and skipped. For duplicate declared identities,
the first successfully loaded path wins and later candidates are skipped.
Hooks follow manager insertion order: explicitly loaded plugins keep their
existing positions, followed by newly discovered plugins in sorted path order.
This is deterministic for a fixed directory/configuration on one platform,
not a locale-specific or cross-filesystem Unicode collation guarantee.

Discovery invokes the same loader used by explicit loads, so manifest bounds,
disable policy, unique identity checks, entry-path validation, runtime loading,
and handle registration have one implementation. Discovery logs and skips an
individual load error; explicit loading returns it to the caller.

## Test fixture execution

Unix shell worker fixtures run through `/bin/sh` with the script passed as an
argument. The [September 30 release gate](qualification/archive-release-integrated.json)
retains an initial Linux `ETXTBSY` failure from directly executing a freshly
written fixture, followed by passing macOS and Linux gates. The production worker
executable, arguments, environment, process group and pipe setup are unchanged.
This finding does not explain the earlier intermittent response-kind failure.
