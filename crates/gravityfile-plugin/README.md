# gravityfile-plugin

Lua plugins for [gravityfile](https://github.com/epistates/gravityfile).

This crate exists for the `grav` binary and its sibling crates. Its API
follows their needs and can change in any minor release. Writing plugins is
documented in the [plugin guide](https://github.com/epistates/gravityfile/blob/main/docs/PLUGINS.md).

Plugins are Lua scripts that subscribe to `Hook`s (startup, navigation, scan
completion, deletion, ...). The `PluginManager` dispatches hooks to a
`PluginRuntime`; in the application that runtime is a `ProcessRuntime`,
which runs each plugin in its own supervised worker process with deadlines
and bounded IPC, so a misbehaving plugin cannot block or crash the host.
`lua::LuaRuntime` is the in-process engine the worker uses, with a memory
limit, an instruction-count deadline and a sandboxed standard library.

A hook result can show notices in the status line, and can veto the default
action (currently deletion) through `prevent_default`:

```lua
return {
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

## Licence

MIT or Apache-2.0, at your option.
