# Plugins

The explorer runs Lua 5.4 plugins. Each plugin can react to what happens in
the explorer, show a message, and refuse a delete.

Every plugin runs in a supervised worker process. A hook call has a deadline,
messages between the worker and the explorer are size-limited, and a plugin
that hangs or crashes is stopped without affecting the explorer.

## Installing

Put a `.lua` file in the plugin directory:

- Linux and macOS: `${XDG_CONFIG_HOME:-~/.config}/gravityfile/plugins/`
- Windows: `%AppData%\gravityfile\plugins`
- With `GRAVITYFILE_HOME` set: `$GRAVITYFILE_HOME/config/plugins/`

Plugins are loaded when the explorer starts. If one cannot be loaded, the
status line says so.

## Writing a plugin

A plugin returns a table of hook functions. Each is called with the plugin
table, the hook's fields, and the context:

```lua
return {
  on_scan_complete = function(self, hook, ctx)
    gf.notify(string.format("%d files, %d bytes", hook.total_files, hook.total_size))
  end,

  -- Awaited before deleting; returning prevent_default stops the delete.
  on_delete_start = function(self, hook, ctx)
    for _, path in ipairs(hook.items) do
      if path:match("/%.git$") then
        gf.notify("Not deleting " .. path, "error")
        return { prevent_default = true }
      end
    end
  end,
}
```

See [`examples/plugins/lua/guard.lua`](../examples/plugins/lua/guard.lua).

## Hooks

| Hook | Fields | When |
|---|---|---|
| `on_startup` | | The explorer opened |
| `on_navigate` | `from`, `to` | The explorer moved to another directory |
| `on_scan_start` | `path` | A full scan started |
| `on_scan_complete` | `path`, `total_files`, `total_dirs`, `total_size` | A full scan finished |
| `on_scan_error` | `path`, `error` | A full scan failed |
| `on_delete_start` | `items`, `use_trash` | Before deleting; can refuse |
| `on_delete_complete` | `deleted`, `failed`, `bytes_freed` | A delete finished |
| `on_copy_complete` | `succeeded`, `failed`, `bytes_copied` | A copy finished |
| `on_move_complete` | `succeeded`, `failed` | A move finished |
| `on_rename_complete` | `source`, `new_path` | An entry was renamed |
| `on_duplicates_found` | `group_count`, `wasted_bytes` | Duplicate analysis finished |
| `on_age_analysis_complete` | `stale_dirs`, `files`, `average_age_secs` | Age analysis finished |
| `on_shutdown` | | The explorer is closing |

Hooks run in the background and never hold up the explorer, with two
exceptions. `on_delete_start` is awaited, because a plugin can refuse the
delete by returning `{ prevent_default = true }`. `on_shutdown` is awaited so
plugins can finish before the explorer exits.

The context `ctx` has:

- `cwd`: the scanned directory
- `view_root`: the directory being viewed
- `theme`: the colour theme's name
- `selected_node`: the selected entry's `path`, `name`, `kind`, `size`,
  `timestamps`, `git_status` and `child_count`, when something is selected

## API

### `gf`

- `gf.notify(message, level)`: show `message` in the status line, prefixed
  with the plugin's name. `level` is `"info"` (the default), `"warn"` or
  `"error"`. A hook call can raise at most 8 messages, each up to 500 bytes.
- `gf.log_info(message)`, `gf.log_warn(message)`, `gf.log_error(message)`:
  write to the log file given by `--log-file`.
- `gf.version`: the gravityfile version.

### `fs`

Files below the scanned directory can be read, up to a size limit:

- `fs.read(path, limit)`, `fs.read_bytes(path, limit)`
- `fs.exists(path)`, `fs.is_dir(path)`, `fs.is_file(path)`
- `fs.metadata(path)`: `size`, `is_dir`, `is_file`, `is_symlink`,
  `readonly`, `modified`, `created`
- `fs.extension(path)`, `fs.filename(path)`, `fs.parent(path)`,
  `fs.join(...)`

## What plugins can do

Plugins run without Lua's `io`, `os`, `debug`, `load` and `package`
libraries, so they cannot write files, run programs or open connections, and
can read files only through `fs`, below the scanned directory. Hook calls are
serialized and limited by a deadline and an instruction budget.

These restrictions apply within Lua, inside a separate process; they are not
operating-system confinement. Install only plugins you trust.
