use std::path::PathBuf;
use std::time::{Duration, Instant};

use gravityfile_plugin::{
    Hook, HookContext, PluginConfig, PluginRuntime, ProcessRuntime, SandboxConfig, Value,
};
use tokio_util::sync::CancellationToken;

fn runtime(language: &str, timeout_ms: u64) -> ProcessRuntime {
    let mut runtime =
        ProcessRuntime::new(PathBuf::from(env!("CARGO_BIN_EXE_grav")), language).unwrap();
    runtime
        .init(&PluginConfig::default().with_timeout(timeout_ms))
        .unwrap();
    runtime
}

#[test]
fn oversized_multibyte_worker_logs_do_not_break_following_replies() {
    let root = tempfile::tempdir().unwrap();
    let source = root.path().join("logging.lua");
    std::fs::write(
        &source,
        r#"return {
        on_startup = function(self, hook)
            gf.notify(string.rep("界", 40000))
            return { value = 11 }
        end
    }"#,
    )
    .unwrap();
    let mut runtime = runtime("lua", 2000);
    let handle = runtime.load_plugin("logging", &source).unwrap();
    for _ in 0..2 {
        let result = runtime
            .call_hook_sync(handle, &Hook::OnStartup, &HookContext::new())
            .unwrap();
        assert_eq!(result.value.unwrap().as_i64(), Some(11));
    }
    runtime.unload_plugin(handle).unwrap();
}

#[tokio::test]
async fn plugin_state_persists_and_hooks_work_across_ipc() {
    let root = tempfile::tempdir().unwrap();
    let source = root.path().join("counter.lua");
    std::fs::write(&source, r#"return {
        counter = 0,
        bump = function(self, n) self.counter = self.counter + n; return self.counter end,
        on_startup = function(self, hook) gf.notify("worker notification"); return { handled = true, value = 42 } end
    }"#).unwrap();
    let mut runtime = runtime("lua", 2000);
    let handle = runtime.load_plugin("counter", &source).unwrap();
    assert!(runtime.has_hook(handle, "on_startup"));
    assert_eq!(
        runtime
            .call_method(handle, "bump", vec![Value::Integer(2)])
            .await
            .unwrap()
            .as_i64(),
        Some(2)
    );
    assert_eq!(
        runtime
            .call_method(handle, "bump", vec![Value::Integer(3)])
            .await
            .unwrap()
            .as_i64(),
        Some(5)
    );
    let hook = runtime
        .call_hook_sync(handle, &Hook::OnStartup, &HookContext::new())
        .unwrap();
    assert_eq!(hook.value.unwrap().as_i64(), Some(42));
    runtime.unload_plugin(handle).unwrap();
    assert!(runtime.loaded_plugins().is_empty());
}

#[test]
fn runaway_load_is_terminated_without_harming_another_plugin() {
    let root = tempfile::tempdir().unwrap();
    let good = root.path().join("good.lua");
    let bad = root.path().join("bad.lua");
    std::fs::write(
        &good,
        "return { on_startup = function(self, hook) return { value = 7 } end }",
    )
    .unwrap();
    std::fs::write(&bad, "while true do end").unwrap();
    let mut runtime = runtime("lua", 500);
    let healthy = runtime.load_plugin("healthy", &good).unwrap();
    let started = Instant::now();
    assert!(runtime.load_plugin("runaway", &bad).is_err());
    assert!(started.elapsed() < Duration::from_secs(5));
    assert_eq!(
        runtime
            .call_hook_sync(healthy, &Hook::OnStartup, &HookContext::new())
            .unwrap()
            .value
            .unwrap()
            .as_i64(),
        Some(7)
    );
}

#[tokio::test]
async fn isolated_workers_support_both_languages() {
    for (language, code) in [("lua", "return 6 * 7"), ("rhai", "6 * 7")] {
        let runtime = runtime(language, 2000);
        let mut sandbox = SandboxConfig::minimal();
        sandbox.timeout_ms = 2000;
        let mut context = runtime.create_isolated_context(&sandbox).unwrap();
        assert_eq!(
            context
                .execute(code.as_bytes(), CancellationToken::new())
                .await
                .unwrap()
                .as_i64(),
            Some(42)
        );
        context.set_global("answer", Value::Integer(17)).unwrap();
        assert_eq!(context.get_global("answer").unwrap().as_i64(), Some(17));
        let expression = if language == "lua" {
            "return answer + 1"
        } else {
            "answer + 1"
        };
        assert_eq!(
            context
                .execute(expression.as_bytes(), CancellationToken::new())
                .await
                .unwrap()
                .as_i64(),
            Some(18)
        );
    }
}

#[tokio::test]
async fn cancellation_kills_the_isolate_and_requires_a_fresh_context() {
    let runtime = runtime("lua", 5000);
    let mut sandbox = SandboxConfig::minimal();
    sandbox.timeout_ms = 5000;
    let context = runtime.create_isolated_context(&sandbox).unwrap();
    let token = CancellationToken::new();
    let cancel = token.clone();
    let task = async move {
        tokio::time::sleep(Duration::from_millis(50)).await;
        cancel.cancel();
    };
    let (result, ()) = tokio::join!(context.execute(b"while true do end", token), task);
    assert!(matches!(
        result,
        Err(gravityfile_plugin::PluginError::Cancelled { .. })
    ));
    assert!(context.get_global("anything").is_err());
}

#[tokio::test]
async fn oversized_results_fail_without_poisoning_other_workers() {
    let runtime = runtime("lua", 2000);
    let context = runtime
        .create_isolated_context(&SandboxConfig::minimal())
        .unwrap();
    assert!(
        context
            .execute(
                b"return string.rep('x', 5 * 1024 * 1024)",
                CancellationToken::new()
            )
            .await
            .is_err()
    );
    assert!(context.get_global("anything").is_err());
    let fresh = runtime
        .create_isolated_context(&SandboxConfig::minimal())
        .unwrap();
    assert_eq!(
        fresh
            .execute(b"return 42", CancellationToken::new())
            .await
            .unwrap()
            .as_i64(),
        Some(42)
    );
}
