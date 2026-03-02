# gravityfile Plugin System

gravityfile features a state-of-the-art, language-agnostic plugin architecture. You can extend the explorer's functionality using **Lua**, **Rhai**, or **WebAssembly (WASM)**.

## Architecture

The plugin system is built on three core pillars:
1. **Event Hooks**: React to file system events (scans, navigation, deletions).
2. **Custom Actions**: Implement new file operations with progress reporting.
3. **Isolated Runtimes**: Background tasks (like previews or heavy analysis) run in restricted sandboxes.

## Supported Runtimes

### 🌙 Lua (Recommended)
Fast, lightweight, and the industry standard for TUI extensibility.
- **Engine**: `mlua` (Lua 5.4)
- **Best for**: UI modifications, event hooks, and simple file operations.

### 🦀 Rhai
A native Rust scripting language with seamless integration.
- **Engine**: `rhai`
- **Best for**: Performance-critical logic and deep integration with Rust types.

### 🕸️ WebAssembly (WASM)
The gold standard for security and portability.
- **Engine**: `extism`
- **Best for**: Complex binary plugins, custom previewers, and untrusted third-party extensions.

---

## Getting Started

Plugins are discovered in the following directories:
- **macOS**: `~/Library/Application Support/gravityfile/plugins/`
- **Linux**: `~/.config/gravityfile/plugins/`
- **Windows**: `%AppData%\gravityfile\plugins`

### Example: Lua Hook (`plugins/notify.lua`)

See [**examples/plugins/lua/git_notify.lua**](../examples/plugins/lua/git_notify.lua) for a more complex example.

```lua
-- Respond to scan completion
function on_scan_complete(self, hook)
    local tree = hook.tree
    gf.notify(string.format("Scan complete: %d files found", tree.stats.total_files), "info")
end

return {
    on_scan_complete = on_scan_complete
}
```

---

## Language-Specific Examples

- **🌙 Lua**: [Git Status Notifier](../examples/plugins/lua/git_notify.lua) - Alerts when entering a directory with untracked files.
- **🦀 Rhai**: [Large File Warning](../examples/plugins/rhai/large_file_warning.rhai) - Logs a warning if a massive file is detected during a scan.
- **🕸️ WASM**: [Rust PDK Example](../examples/plugins/wasm/rust-pdk/src/lib.rs) - A starter template for high-performance binary plugins.

---

## Security & Sandboxing

gravityfile employs a "Security First" approach. Plugins can be restricted using a `SandboxConfig`:

- **Read/Write Caps**: Restrict plugins to specific directory subtrees.
- **Resource Limits**: Enforce memory (e.g., 64MB) and execution time (e.g., 5s) limits.
- **API Permissions**: Opt-in to dangerous APIs like `Execute` or `Network`.

### The `fs` API

The `fs` namespace provides safe, sandboxed access to the filesystem:
- `fs.read(path, limit)`: Read file content (string).
- `fs.read_bytes(path, limit)`: Read raw bytes.
- `fs.exists(path)`: Check existence.
- `fs.metadata(path)`: Get size, timestamps, and permissions.

*Note: In isolated contexts, all `fs` calls are validated against the plugin's `allowed_read_paths`.*

---

## API Reference (Lua)

### `gf` (Global)
- `gf.log_info(msg)` / `gf.log_warn(msg)` / `gf.log_error(msg)`
- `gf.notify(msg, level)`: Show a TUI notification.
- `gf.version`: Current gravityfile version.

### `ui` (User Interface)
- `ui.span(text, style)`: Create a styled text fragment.
- `ui.line({spans})`: Create a line of text.
- `ui.paragraph({lines})`: Create a multi-line text block.
- `ui.colors`: Access theme colors (`colors.red`, `colors.blue`, etc.).

---

## Writing WASM Plugins

WASM plugins must export specific functions following the `Extism` pattern. Input and output are exchanged via JSON-serialized buffers.

**Example Export (Rust/WASM):**
```rust
#[extism_pdk::plugin_fn]
pub fn on_scan_complete(hook: Hook) -> FnResult<HookResult> {
    // Logic here...
    Ok(HookResult::ok())
}
```

## Advanced: Performance Profiling

Plugins are executed asynchronously where possible. For high-latency operations (like network calls or heavy computation), always use the **Isolated Context** to prevent UI hangs.
