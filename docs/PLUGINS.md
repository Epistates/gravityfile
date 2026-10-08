# gravityfile Plugin System

gravityfile features a language-agnostic plugin architecture. You can extend the explorer's functionality using **Lua** or **Rhai**.

The standard CLI/TUI runs each plugin in a supervised child process. See
[process execution and limits](PLUGIN_PROCESSES.md) for deadlines, recovery,
embedding requirements, and the remaining security boundary.

## Architecture

The plugin system is built on three core pieces:
1. **Event Hooks**: React to file system events (scans, navigation, deletions).
2. **Action Hooks**: Observe or veto supported host actions.
3. **Isolated Contexts**: Run standalone Lua/Rhai snippets with a separate restricted runtime.

## Supported Runtimes

### 🌙 Lua (Recommended)
Fast, lightweight, and the industry standard for TUI extensibility.
- **Engine**: `mlua` (Lua 5.4)
- **Best for**: UI modifications, event hooks, and simple file operations.

### 🦀 Rhai
A native Rust scripting language with seamless integration.
- **Engine**: `rhai`
- **Best for**: Performance-critical logic and deep integration with Rust types.

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

---

## Security & Sandboxing

Plugins execute in-process and can be restricted using a `SandboxConfig`:

- **Read/Write Caps**: Restrict plugins to specific directory subtrees.
- **Resource Limits**: Enforce memory (e.g., 64MB) and execution time (e.g., 5s) limits.
- **Deny by Default**: Filesystem access must be declared and is scoped to configured roots.

These runtime controls are defense in depth, not an OS-level sandbox. Only install plugins you trust.

### The `fs` API

The `fs` namespace provides safe, sandboxed access to the filesystem:
- `fs.read(path, limit)`: Read file content (string).
- `fs.read_bytes(path, limit)`: Read raw bytes.
- `fs.exists(path)`: Check existence.
- `fs.metadata(path)`: Get size, timestamps, and permissions.

All exposed `fs` reads are validated against the plugin's allowed roots and maximum read size.

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

## Advanced: Performance Profiling

Main hook execution is serialized because it shares runtime state. Keep hooks short; use an isolated context for standalone CPU-heavy work.
