//! Hosting plugins: loading them, firing hooks, and showing what they report.
//!
//! Observer hooks are fired with [`App::fire`] and run on a task, so a slow
//! plugin never holds up the explorer; their results come back through the
//! run loop. `on_delete_start` is awaited because a plugin can veto it, and
//! `on_shutdown` is awaited so plugins can finish before the explorer exits.

use gravityfile_plugin::{Hook, HookContext, HookResult, NoticeLevel};

use super::App;

impl App {
    /// Start the plugin runtimes and load the installed plugins, reporting
    /// any that could not be loaded.
    pub(super) async fn init_plugins(&mut self) {
        let Some(pm) = self.plugin_manager.clone() else {
            return;
        };
        let mut pm = pm.write().await;
        let mut problems = Vec::new();
        if let Err(error) = pm.init_runtimes() {
            tracing::warn!(%error, "Plugin runtime init error");
            problems.push(error.to_string());
        }
        if let Err(error) = pm.discover_plugins().await {
            tracing::warn!(%error, "Plugin discovery error");
            problems.push(error.to_string());
        }
        self.plugins_loaded = pm.plugins().next().is_some();
        if !problems.is_empty() {
            self.operation_message = Some((
                false,
                format!("Plugins could not all be loaded: {}", problems.join("; ")),
            ));
        }
    }

    /// What every hook is told: where the explorer is, its theme, and the
    /// selected entry's own metadata (not its children, which can be huge).
    fn hook_context(&self) -> HookContext {
        let mut context = HookContext::new()
            .with_cwd(self.path.clone())
            .with_view_root(self.nav.view_root.clone())
            .with_theme(self.theme.name);
        let selected = self.get_selected_path().and_then(|path| {
            let tree = self.tree.as_ref()?;
            Self::find_node_at_path(&tree.root, &path, &tree.root_path).map(|node| (path, node))
        });
        if let Some((path, node)) = selected {
            let shallow = serde_json::json!({
                "path": path.to_string_lossy(),
                "name": node.name,
                "kind": node.kind,
                "size": node.size,
                "blocks": node.blocks,
                "timestamps": node.timestamps,
                "inode": node.inode,
                "git_status": node.git_status,
                "child_count": node.children.len(),
            });
            if let Ok(value) = serde_json::from_value::<gravityfile_plugin::Value>(shallow) {
                context.set("selected_node", value);
            }
        }
        context
    }

    /// Fire an observer hook without waiting for it.
    pub(super) fn fire(&mut self, hook: Hook) {
        let Some(pm) = self.plugin_manager.clone().filter(|_| self.plugins_loaded) else {
            return;
        };
        let context = self.hook_context();
        let sender = self.hook_tx.clone();
        self.hooks_in_flight += 1;
        tokio::spawn(async move {
            let results = pm.read().await.dispatch_hook(&hook, &context).await;
            let _ = sender.send(results);
        });
    }

    /// Fire a hook and wait for every plugin's answer, showing what they
    /// report. For hooks whose result the explorer acts on.
    pub(super) async fn dispatch_and_wait(&mut self, hook: Hook) -> Vec<HookResult> {
        let Some(pm) = self.plugin_manager.clone().filter(|_| self.plugins_loaded) else {
            return Vec::new();
        };
        let context = self.hook_context();
        let results = pm.read().await.dispatch_hook(&hook, &context).await;
        self.show_hook_results(&results);
        results
    }

    /// A fired hook finished.
    pub(super) fn finish_hook(&mut self, results: Vec<HookResult>) {
        self.hooks_in_flight = self.hooks_in_flight.saturating_sub(1);
        self.show_hook_results(&results);
    }

    /// Show plugins' notices and failures in the status line, newest last.
    fn show_hook_results(&mut self, results: &[HookResult]) {
        for result in results {
            let plugin = result.plugin.as_deref().unwrap_or("plugin");
            for notice in &result.notices {
                let ok = notice.level == NoticeLevel::Info;
                self.operation_message = Some((ok, format!("{plugin}: {}", notice.message)));
            }
            if let Some(error) = &result.error {
                self.operation_message = Some((false, format!("{plugin} failed: {error}")));
            }
        }
        self.needs_redraw = true;
    }

    /// Fire `on_navigate` if the explorer moved since `old_root`.
    pub(super) fn check_navigation(&mut self, old_root: &std::path::Path) {
        if old_root != self.nav.view_root {
            self.fire(Hook::OnNavigate {
                from: old_root.to_path_buf(),
                to: self.nav.view_root.clone(),
            });
        }
    }
}
