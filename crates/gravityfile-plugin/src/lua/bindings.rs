//! Rust-to-Lua bindings for the gravityfile API.

use std::io::Read as _;

use mlua::{Lua, Table, Value as LuaValue};

use crate::sandbox::SandboxConfig;
use crate::types::{PluginError, PluginResult};

/// Whether the sandbox permits reading `path`, checking both the literal path
/// and its resolved form.
///
/// `can_read` alone is not enough for existence probes: a symlink inside an
/// allowed directory can point outside it, so the target is re-checked once the
/// path resolves. A path that does not resolve is judged on the literal form.
fn sandbox_allows_read(sandbox: Option<&SandboxConfig>, path: &std::path::Path) -> bool {
    let Some(sandbox) = sandbox else {
        return true;
    };
    if !sandbox.can_read(path) {
        return false;
    }
    match std::fs::canonicalize(path) {
        Ok(resolved) => sandbox.can_read(&resolved),
        Err(_) => true,
    }
}

fn open_readable_file(
    path: &std::path::Path,
    requested_limit: Option<usize>,
    sandbox: Option<&SandboxConfig>,
) -> mlua::Result<Option<(std::fs::File, usize)>> {
    if let Some(sb) = sandbox
        && !sb.can_read(path)
    {
        return Err(mlua::Error::external("Read denied by sandbox policy"));
    }

    let real = match std::fs::canonicalize(path) {
        Ok(real) => real,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(mlua::Error::external(error)),
    };
    if let Some(sb) = sandbox
        && !sb.can_read(&real)
    {
        return Err(mlua::Error::external("Read denied by sandbox policy"));
    }

    let file = std::fs::File::open(&real).map_err(mlua::Error::external)?;
    let metadata = file.metadata().map_err(mlua::Error::external)?;
    if !metadata.is_file() {
        return Err(mlua::Error::external(
            "Read denied: path is not a regular file",
        ));
    }

    let limit = match sandbox {
        Some(sb) => {
            if metadata.len() > sb.max_read_size as u64 {
                return Err(mlua::Error::external(format!(
                    "Read denied: file too large ({} bytes > {} limit)",
                    metadata.len(),
                    sb.max_read_size
                )));
            }
            requested_limit
                .unwrap_or(sb.max_read_size)
                .min(sb.max_read_size)
        }
        None => requested_limit.unwrap_or(1024 * 1024),
    };

    Ok(Some((file, limit)))
}

/// Create the 'fs' (filesystem) namespace.
pub fn create_fs_api(lua: &Lua, sandbox: Option<SandboxConfig>) -> PluginResult<Table> {
    let fs = lua.create_table().map_err(|e| PluginError::LoadError {
        name: "lua".into(),
        message: format!("Failed to create fs table: {}", e),
    })?;

    // fs.read(path, limit?) - Read file contents (capped at source via take())
    let sb_clone1 = sandbox.clone();
    let read = lua
        .create_function(move |lua, (path, limit): (String, Option<usize>)| {
            let path = std::path::Path::new(&path);
            let Some((f, limit)) = open_readable_file(path, limit, sb_clone1.as_ref())? else {
                return Ok(LuaValue::Nil);
            };
            let mut content = String::new();
            f.take(limit as u64)
                .read_to_string(&mut content)
                .map_err(mlua::Error::external)?;

            Ok(LuaValue::String(lua.create_string(content.as_str())?))
        })
        .map_err(|e| PluginError::LoadError {
            name: "lua".into(),
            message: e.to_string(),
        })?;
    fs.set("read", read).ok();

    // fs.read_bytes(path, limit?) - Read file as bytes
    let sb_clone2 = sandbox.clone();
    let read_bytes = lua
        .create_function(move |lua, (path, limit): (String, Option<usize>)| {
            let path = std::path::Path::new(&path);
            let Some((f, limit)) = open_readable_file(path, limit, sb_clone2.as_ref())? else {
                return Ok(LuaValue::Nil);
            };
            let mut buf = Vec::new();
            f.take(limit as u64)
                .read_to_end(&mut buf)
                .map_err(mlua::Error::external)?;

            Ok(LuaValue::String(lua.create_string(&buf)?))
        })
        .map_err(|e| PluginError::LoadError {
            name: "lua".into(),
            message: e.to_string(),
        })?;
    fs.set("read_bytes", read_bytes).ok();

    // fs.exists(path) - Check if path exists
    let sb_clone3 = sandbox.clone();
    let exists = lua
        .create_function(move |_, path: String| {
            let path = std::path::Path::new(&path);
            if !sandbox_allows_read(sb_clone3.as_ref(), path) {
                return Ok(false); // Hide existence if not allowed to read
            }
            Ok(path.exists())
        })
        .map_err(|e| PluginError::LoadError {
            name: "lua".into(),
            message: e.to_string(),
        })?;
    fs.set("exists", exists).ok();

    // fs.is_dir(path) - Check if path is a directory
    let sb_clone4 = sandbox.clone();
    let is_dir = lua
        .create_function(move |_, path: String| {
            let path = std::path::Path::new(&path);
            if !sandbox_allows_read(sb_clone4.as_ref(), path) {
                return Ok(false);
            }
            Ok(path.is_dir())
        })
        .map_err(|e| PluginError::LoadError {
            name: "lua".into(),
            message: e.to_string(),
        })?;
    fs.set("is_dir", is_dir).ok();

    // fs.is_file(path) - Check if path is a file
    let sb_clone5 = sandbox.clone();
    let is_file = lua
        .create_function(move |_, path: String| {
            let path = std::path::Path::new(&path);
            if !sandbox_allows_read(sb_clone5.as_ref(), path) {
                return Ok(false);
            }
            Ok(path.is_file())
        })
        .map_err(|e| PluginError::LoadError {
            name: "lua".into(),
            message: e.to_string(),
        })?;
    fs.set("is_file", is_file).ok();

    // fs.metadata(path) - Get file metadata
    let sb_clone6 = sandbox.clone();
    let metadata = lua
        .create_function(move |lua, path: String| {
            let path = std::path::Path::new(&path);

            if !sandbox_allows_read(sb_clone6.as_ref(), path) {
                return Err(mlua::Error::external("Read denied by sandbox policy"));
            }

            let meta = match std::fs::symlink_metadata(path) {
                Ok(metadata) => metadata,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    return Ok(LuaValue::Nil);
                }
                Err(error) => return Err(mlua::Error::external(error)),
            };

            let table = lua.create_table()?;
            table.set("size", meta.len())?;
            table.set("is_dir", meta.is_dir())?;
            table.set("is_file", meta.is_file())?;
            table.set("is_symlink", meta.is_symlink())?;
            table.set("readonly", meta.permissions().readonly())?;

            if let Ok(modified) = meta.modified()
                && let Ok(duration) = modified.duration_since(std::time::UNIX_EPOCH)
            {
                table.set("modified", duration.as_secs())?;
            }

            if let Ok(created) = meta.created()
                && let Ok(duration) = created.duration_since(std::time::UNIX_EPOCH)
            {
                table.set("created", duration.as_secs())?;
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lua_reads_cannot_override_the_sandbox_size_limit() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("large.txt");
        std::fs::write(&path, b"more than four bytes").unwrap();

        let mut sandbox = SandboxConfig::minimal().allow_read(temp.path());
        sandbox.max_read_size = 4;
        let lua = Lua::new();
        let fs = create_fs_api(&lua, Some(sandbox)).unwrap();
        lua.globals().set("fs", fs).unwrap();
        lua.globals()
            .set("path", path.to_string_lossy().as_ref())
            .unwrap();

        let error = lua
            .load("return fs.read(path, 1000000)")
            .eval::<String>()
            .unwrap_err();
        assert!(error.to_string().contains("file too large"));
    }

    #[cfg(unix)]
    #[test]
    fn lua_metadata_reports_symbolic_links_without_following_them() {
        let temp = tempfile::tempdir().unwrap();
        let target = temp.path().join("target.txt");
        let link = temp.path().join("target-link");
        std::fs::write(&target, "target").unwrap();
        std::os::unix::fs::symlink(&target, &link).unwrap();

        let sandbox = SandboxConfig::minimal().allow_read(temp.path());
        let lua = Lua::new();
        let fs = create_fs_api(&lua, Some(sandbox)).unwrap();
        lua.globals().set("fs", fs).unwrap();
        lua.globals()
            .set("path", link.to_string_lossy().as_ref())
            .unwrap();

        let is_symlink = lua
            .load("return fs.metadata(path).is_symlink")
            .eval::<bool>()
            .unwrap();
        assert!(is_symlink);
    }
}
