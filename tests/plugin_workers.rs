use std::path::PathBuf;
use std::time::{Duration, Instant};

use gravityfile_plugin::{
    Hook, HookContext, Notice, NoticeLevel, PluginConfig, PluginRuntime, ProcessRuntime,
    SandboxConfig, Value,
};
use tokio_util::sync::CancellationToken;

fn runtime(timeout_ms: u64) -> ProcessRuntime {
    let mut runtime = ProcessRuntime::new(PathBuf::from(env!("CARGO_BIN_EXE_grav"))).unwrap();
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
    let mut runtime = runtime(2000);
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
    let mut runtime = runtime(2000);
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
    assert_eq!(
        hook.notices,
        [Notice::new(NoticeLevel::Info, "worker notification")],
        "gf.notify crosses the process boundary"
    );
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
    let mut runtime = runtime(500);
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
async fn isolated_workers_execute_code_and_keep_globals() {
    let runtime = runtime(2000);
    let mut sandbox = SandboxConfig::minimal();
    sandbox.timeout_ms = 2000;
    let mut context = runtime.create_isolated_context(&sandbox).unwrap();
    assert_eq!(
        context
            .execute(b"return 6 * 7", CancellationToken::new())
            .await
            .unwrap()
            .as_i64(),
        Some(42)
    );
    context.set_global("answer", Value::Integer(17)).unwrap();
    assert_eq!(context.get_global("answer").unwrap().as_i64(), Some(17));
    assert_eq!(
        context
            .execute(b"return answer + 1", CancellationToken::new())
            .await
            .unwrap()
            .as_i64(),
        Some(18)
    );
}

#[tokio::test]
async fn cancellation_kills_the_isolate_and_requires_a_fresh_context() {
    let runtime = runtime(5000);
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
    let runtime = runtime(2000);
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

#[test]
fn hook_context_reaches_plugins_in_the_worker() {
    let root = tempfile::tempdir().unwrap();
    let source = root.path().join("where.lua");
    std::fs::write(
        &source,
        r#"return {
        on_navigate = function(self, hook, ctx)
            gf.notify("now in " .. ctx.view_root, "warn")
            return { value = ctx.theme }
        end
    }"#,
    )
    .unwrap();
    let mut runtime = runtime(2000);
    let handle = runtime.load_plugin("where", &source).unwrap();
    let context = HookContext::new()
        .with_view_root(PathBuf::from("/home/me"))
        .with_theme("nord");
    let hook = Hook::OnNavigate {
        from: PathBuf::from("/home"),
        to: PathBuf::from("/home/me"),
    };
    let result = runtime.call_hook_sync(handle, &hook, &context).unwrap();
    assert_eq!(result.value, Some(Value::String("nord".into())));
    assert_eq!(
        result.notices,
        [Notice::new(NoticeLevel::Warn, "now in /home/me")]
    );
    runtime.unload_plugin(handle).unwrap();
}

#[test]
fn the_example_guard_plugin_refuses_to_delete_git_history() {
    let source = PathBuf::from(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/examples/plugins/lua/guard.lua"
    ));
    let mut runtime = runtime(2000);
    let handle = runtime.load_plugin("guard", &source).unwrap();
    let delete = |items: &[&str]| Hook::OnDeleteStart {
        items: items.iter().map(PathBuf::from).collect(),
        use_trash: true,
    };
    let refused = runtime
        .call_hook_sync(
            handle,
            &delete(&["/work/a.log", "/work/repo/.git"]),
            &HookContext::new(),
        )
        .unwrap();
    assert!(refused.prevent_default);
    assert_eq!(refused.notices[0].level, NoticeLevel::Error);
    let allowed = runtime
        .call_hook_sync(handle, &delete(&["/work/a.log"]), &HookContext::new())
        .unwrap();
    assert!(!allowed.prevent_default && allowed.notices.is_empty());
    let scanned = Hook::OnScanComplete {
        path: PathBuf::from("/work"),
        total_files: 3,
        total_dirs: 1,
        total_size: 2_500_000_000,
    };
    let summary = runtime
        .call_hook_sync(handle, &scanned, &HookContext::new())
        .unwrap();
    assert_eq!(summary.notices[0].message, "Scanned 3 files, 2.5 GB");
    runtime.unload_plugin(handle).unwrap();
}
