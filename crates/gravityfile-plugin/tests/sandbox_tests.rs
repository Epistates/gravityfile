//! Sandbox enforcement guarantees for the isolated plugin runtime.

use std::time::Instant;

use gravityfile_plugin::{PluginRuntime, SandboxConfig, Value};
use tokio_util::sync::CancellationToken;

fn isolate(sandbox: SandboxConfig) -> Box<dyn gravityfile_plugin::IsolatedContext> {
    let runtime = gravityfile_plugin::lua::LuaRuntime::new().expect("lua runtime");
    runtime
        .create_isolated_context(&sandbox)
        .expect("isolated context")
}

async fn eval(context: &dyn gravityfile_plugin::IsolatedContext, code: &str) -> Value {
    context
        .execute(code.as_bytes(), CancellationToken::new())
        .await
        .expect("evaluation succeeded")
}

fn is_nil_type(value: &Value) -> bool {
    matches!(value, Value::String(s) if s == "nil")
}

#[tokio::test]
async fn dangerous_stdlib_modules_are_never_loaded() {
    let context = isolate(SandboxConfig::minimal());

    for module in ["io", "os", "debug", "package"] {
        let value = eval(&*context, &format!("return type({module})")).await;
        assert!(
            is_nil_type(&value),
            "`{module}` must not exist in a sandboxed VM, got {value:?}"
        );
    }
}

#[tokio::test]
async fn code_loading_and_stdout_writing_globals_are_unavailable() {
    let context = isolate(SandboxConfig::minimal());

    for global in ["load", "loadstring", "loadfile", "dofile", "require"] {
        let value = eval(&*context, &format!("return type({global})")).await;
        assert!(
            is_nil_type(&value),
            "`{global}` must not be callable, got {value:?}"
        );
    }
}

#[tokio::test]
async fn print_is_redirected_and_cannot_reach_stdout() {
    let context = isolate(SandboxConfig::minimal());

    // `print` stays callable so ordinary plugins keep working, but it is a
    // host-provided function that logs rather than the stdlib one.
    let value = eval(&*context, "return type(print)").await;
    assert!(
        matches!(&value, Value::String(s) if s == "function"),
        "print should remain callable, got {value:?}"
    );
    // Calling it must not panic or write to the terminal.
    eval(&*context, "print('into the log', 1, 2) return true").await;
}

#[tokio::test]
async fn the_usable_standard_library_still_works() {
    let context = isolate(SandboxConfig::minimal());

    let value = eval(
        &*context,
        "local t = {3, 1, 2} table.sort(t) return string.format('%d%d%d', t[1], t[2], t[3])",
    )
    .await;
    assert!(
        matches!(&value, Value::String(s) if s == "123"),
        "{value:?}"
    );

    let value = eval(&*context, "return math.floor(7 / 2)").await;
    assert!(matches!(value, Value::Integer(3)), "{value:?}");

    // pcall/type/ipairs come from the base library and must remain available.
    let value = eval(&*context, "return pcall(function() return 1 end)").await;
    assert!(matches!(value, Value::Bool(true)), "{value:?}");
}

#[tokio::test]
async fn the_wall_clock_timeout_bounds_a_blocking_host_call() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("data.bin");
    std::fs::write(&path, vec![0u8; 4 * 1024 * 1024]).unwrap();

    let mut sandbox = SandboxConfig::minimal().allow_read(temp.path());
    sandbox.timeout_ms = 20;
    sandbox.max_read_size = 64 * 1024 * 1024;

    let context = isolate(sandbox);
    context
        .execute(
            format!("PATH = {:?}", path.to_string_lossy()).as_bytes(),
            CancellationToken::new(),
        )
        .await
        .unwrap();

    // Repeated host-side file reads: the instruction hook cannot fire inside
    // them, so only the wall-clock timeout can stop this.
    let start = Instant::now();
    let result = context
        .execute(
            b"local n = 0 for _ = 1, 400 do n = n + #fs.read_bytes(PATH) end return n",
            CancellationToken::new(),
        )
        .await;
    let elapsed = start.elapsed();

    assert!(
        result.is_err(),
        "a 20ms budget must not allow the call to complete (took {elapsed:?})"
    );
    assert!(
        elapsed.as_millis() < 2_000,
        "timeout did not preempt the host call: {elapsed:?}"
    );
}

#[tokio::test]
async fn runaway_bytecode_is_still_aborted_by_the_instruction_hook() {
    let mut sandbox = SandboxConfig::minimal();
    sandbox.timeout_ms = 50;
    let context = isolate(sandbox);

    context
        .execute(
            b"function runaway() while true do end end",
            CancellationToken::new(),
        )
        .await
        .unwrap();

    let result = context
        .call_function("runaway", vec![], CancellationToken::new())
        .await;
    assert!(result.is_err(), "an infinite loop must be aborted");
}

#[cfg(unix)]
#[tokio::test]
async fn existence_probes_do_not_leak_through_a_symlink_out_of_the_sandbox() {
    let temp = tempfile::tempdir().unwrap();
    let allowed = temp.path().join("allowed");
    let secret_dir = temp.path().join("secret");
    std::fs::create_dir_all(&allowed).unwrap();
    std::fs::create_dir_all(&secret_dir).unwrap();
    let secret = secret_dir.join("password.txt");
    std::fs::write(&secret, "hunter2").unwrap();

    // A symlink inside the readable root pointing at a file outside it.
    let escape = allowed.join("escape");
    std::os::unix::fs::symlink(&secret, &escape).unwrap();

    let context = isolate(SandboxConfig::minimal().allow_read(&allowed));
    context
        .execute(
            format!("ESCAPE = {:?}", escape.to_string_lossy()).as_bytes(),
            CancellationToken::new(),
        )
        .await
        .unwrap();

    let exists = eval(&*context, "return fs.exists(ESCAPE)").await;
    assert!(
        matches!(exists, Value::Bool(false)),
        "existence outside the sandbox must not be observable, got {exists:?}"
    );

    let is_file = eval(&*context, "return fs.is_file(ESCAPE)").await;
    assert!(
        matches!(is_file, Value::Bool(false)),
        "file-ness outside the sandbox must not be observable, got {is_file:?}"
    );
}
