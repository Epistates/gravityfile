//! Rust-to-Lua bindings for the gravityfile API.

use mlua::{Lua, Table, Value as LuaValue};

use crate::types::{PluginError, PluginResult};

/// Create the 'fs' (filesystem) namespace.
pub fn create_fs_api(lua: &Lua) -> PluginResult<Table> {
    let fs = lua.create_table().map_err(|e| PluginError::LoadError {
        name: "lua".into(),
        message: format!("Failed to create fs table: {}", e),
    })?;

    // fs.read(path, limit?) - Read file contents
    let read = lua
        .create_function(|lua, (path, limit): (String, Option<usize>)| {
            let limit = limit.unwrap_or(1024 * 1024); // 1MB default
            let path = std::path::Path::new(&path);

            if !path.exists() {
                return Ok(LuaValue::Nil);
            }

            let content = std::fs::read_to_string(path).map_err(|e| mlua::Error::external(e))?;

            let truncated = if content.len() > limit {
                &content[..limit]
            } else {
                &content
            };

            Ok(LuaValue::String(lua.create_string(truncated)?))
        })
        .map_err(|e| PluginError::LoadError {
            name: "lua".into(),
            message: e.to_string(),
        })?;
    fs.set("read", read).ok();

    // fs.read_bytes(path, limit?) - Read file as bytes
    let read_bytes = lua
        .create_function(|lua, (path, limit): (String, Option<usize>)| {
            let limit = limit.unwrap_or(1024 * 1024);
            let path = std::path::Path::new(&path);

            if !path.exists() {
                return Ok(LuaValue::Nil);
            }

            let content = std::fs::read(path).map_err(|e| mlua::Error::external(e))?;

            let truncated = if content.len() > limit {
                &content[..limit]
            } else {
                &content
            };

            Ok(LuaValue::String(lua.create_string(truncated)?))
        })
        .map_err(|e| PluginError::LoadError {
            name: "lua".into(),
            message: e.to_string(),
        })?;
    fs.set("read_bytes", read_bytes).ok();

    // fs.exists(path) - Check if path exists
    let exists = lua
        .create_function(|_, path: String| {
            Ok(std::path::Path::new(&path).exists())
        })
        .map_err(|e| PluginError::LoadError {
            name: "lua".into(),
            message: e.to_string(),
        })?;
    fs.set("exists", exists).ok();

    // fs.is_dir(path) - Check if path is a directory
    let is_dir = lua
        .create_function(|_, path: String| {
            Ok(std::path::Path::new(&path).is_dir())
        })
        .map_err(|e| PluginError::LoadError {
            name: "lua".into(),
            message: e.to_string(),
        })?;
    fs.set("is_dir", is_dir).ok();

    // fs.is_file(path) - Check if path is a file
    let is_file = lua
        .create_function(|_, path: String| {
            Ok(std::path::Path::new(&path).is_file())
        })
        .map_err(|e| PluginError::LoadError {
            name: "lua".into(),
            message: e.to_string(),
        })?;
    fs.set("is_file", is_file).ok();

    // fs.metadata(path) - Get file metadata
    let metadata = lua
        .create_function(|lua, path: String| {
            let path = std::path::Path::new(&path);

            if !path.exists() {
                return Ok(LuaValue::Nil);
            }

            let meta = std::fs::metadata(path).map_err(|e| mlua::Error::external(e))?;

            let table = lua.create_table()?;
            table.set("size", meta.len())?;
            table.set("is_dir", meta.is_dir())?;
            table.set("is_file", meta.is_file())?;
            table.set("is_symlink", meta.is_symlink())?;
            table.set("readonly", meta.permissions().readonly())?;

            if let Ok(modified) = meta.modified() {
                if let Ok(duration) = modified.duration_since(std::time::UNIX_EPOCH) {
                    table.set("modified", duration.as_secs())?;
                }
            }

            if let Ok(created) = meta.created() {
                if let Ok(duration) = created.duration_since(std::time::UNIX_EPOCH) {
                    table.set("created", duration.as_secs())?;
                }
            }

            Ok(LuaValue::Table(table))
        })
        .map_err(|e| PluginError::LoadError {
            name: "lua".into(),
            message: e.to_string(),
        })?;
    fs.set("metadata", metadata).ok();

    // fs.extension(path) - Get file extension
    let extension = lua
        .create_function(|lua, path: String| {
            let path = std::path::Path::new(&path);
            match path.extension().and_then(|e| e.to_str()) {
                Some(ext) => Ok(LuaValue::String(lua.create_string(ext)?)),
                None => Ok(LuaValue::Nil),
            }
        })
        .map_err(|e| PluginError::LoadError {
            name: "lua".into(),
            message: e.to_string(),
        })?;
    fs.set("extension", extension).ok();

    // fs.filename(path) - Get filename from path
    let filename = lua
        .create_function(|lua, path: String| {
            let path = std::path::Path::new(&path);
            match path.file_name().and_then(|n| n.to_str()) {
                Some(name) => Ok(LuaValue::String(lua.create_string(name)?)),
                None => Ok(LuaValue::Nil),
            }
        })
        .map_err(|e| PluginError::LoadError {
            name: "lua".into(),
            message: e.to_string(),
        })?;
    fs.set("filename", filename).ok();

    // fs.parent(path) - Get parent directory
    let parent = lua
        .create_function(|lua, path: String| {
            let path = std::path::Path::new(&path);
            match path.parent().and_then(|p| p.to_str()) {
                Some(parent) => Ok(LuaValue::String(lua.create_string(parent)?)),
                None => Ok(LuaValue::Nil),
            }
        })
        .map_err(|e| PluginError::LoadError {
            name: "lua".into(),
            message: e.to_string(),
        })?;
    fs.set("parent", parent).ok();

    // fs.join(path, ...) - Join path components
    let join = lua
        .create_function(|lua, paths: mlua::Variadic<String>| {
            let mut result = std::path::PathBuf::new();
            for p in paths {
                result.push(p);
            }
            Ok(LuaValue::String(
                lua.create_string(result.to_string_lossy().as_ref())?,
            ))
        })
        .map_err(|e| PluginError::LoadError {
            name: "lua".into(),
            message: e.to_string(),
        })?;
    fs.set("join", join).ok();

    Ok(fs)
}

/// Create the 'ui' (user interface) namespace.
pub fn create_ui_api(lua: &Lua) -> PluginResult<Table> {
    let ui = lua.create_table().map_err(|e| PluginError::LoadError {
        name: "lua".into(),
        message: format!("Failed to create ui table: {}", e),
    })?;

    // ui.span(text, style?) - Create a styled text span
    let span = lua
        .create_function(|lua, (text, style): (String, Option<Table>)| {
            let table = lua.create_table()?;
            table.set("type", "span")?;
            table.set("text", text)?;

            if let Some(s) = style {
                table.set("style", s)?;
            }

            Ok(table)
        })
        .map_err(|e| PluginError::LoadError {
            name: "lua".into(),
            message: e.to_string(),
        })?;
    ui.set("span", span).ok();

    // ui.line(spans) - Create a line from spans
    let line = lua
        .create_function(|lua, spans: Table| {
            let table = lua.create_table()?;
            table.set("type", "line")?;
            table.set("spans", spans)?;
            Ok(table)
        })
        .map_err(|e| PluginError::LoadError {
            name: "lua".into(),
            message: e.to_string(),
        })?;
    ui.set("line", line).ok();

    // ui.paragraph(lines) - Create a paragraph
    let paragraph = lua
        .create_function(|lua, lines: Table| {
            let table = lua.create_table()?;
            table.set("type", "paragraph")?;
            table.set("lines", lines)?;
            Ok(table)
        })
        .map_err(|e| PluginError::LoadError {
            name: "lua".into(),
            message: e.to_string(),
        })?;
    ui.set("paragraph", paragraph).ok();

    // ui.style(opts) - Create a style
    let style_fn = lua
        .create_function(|lua, opts: Table| {
            let table = lua.create_table()?;
            table.set("type", "style")?;

            // Copy style options
            for key in ["fg", "bg", "bold", "italic", "underline", "dim"] {
                if let Ok(val) = opts.get::<LuaValue>(key) {
                    if val != LuaValue::Nil {
                        table.set(key, val)?;
                    }
                }
            }

            Ok(table)
        })
        .map_err(|e| PluginError::LoadError {
            name: "lua".into(),
            message: e.to_string(),
        })?;
    ui.set("style", style_fn).ok();

    // Predefined colors
    let colors = lua.create_table().map_err(|e| PluginError::LoadError {
        name: "lua".into(),
        message: e.to_string(),
    })?;

    for (name, value) in [
        ("black", "black"),
        ("red", "red"),
        ("green", "green"),
        ("yellow", "yellow"),
        ("blue", "blue"),
        ("magenta", "magenta"),
        ("cyan", "cyan"),
        ("white", "white"),
        ("gray", "gray"),
        ("dark_gray", "dark_gray"),
        ("light_red", "light_red"),
        ("light_green", "light_green"),
        ("light_yellow", "light_yellow"),
        ("light_blue", "light_blue"),
        ("light_magenta", "light_magenta"),
        ("light_cyan", "light_cyan"),
    ] {
        colors.set(name, value).ok();
    }

    ui.set("colors", colors).ok();

    Ok(ui)
}
