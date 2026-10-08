//! Where gravityfile keeps its own files.
//!
//! XDG layout on every Unix, macOS included (`~/.config/gravityfile`,
//! `~/.cache/gravityfile`, ...), honouring the `XDG_*_HOME` variables; the
//! usual application directories on Windows. `GRAVITYFILE_HOME` puts
//! everything under one directory, for portable installs and tests.

use std::path::{Path, PathBuf};

use etcetera::BaseStrategy;

const APP: &str = "gravityfile";

/// gravityfile's directories.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Dirs {
    /// Settings and plugins.
    pub config: PathBuf,
    /// Data that can be rebuilt.
    pub cache: PathBuf,
    /// Data worth keeping, such as saved scans.
    pub data: PathBuf,
    /// Logs and other state.
    pub state: PathBuf,
}

impl Dirs {
    /// gravityfile's directories for this user, or `None` without a home
    /// directory. Reads only; see [`migrate_legacy_config`].
    pub fn discover() -> Option<Self> {
        if let Some(home) = home_override() {
            return Some(Self::under(&home));
        }
        let base = etcetera::choose_base_strategy().ok()?;
        let config = base.config_dir().join(APP);
        Some(Self {
            config: match legacy_config() {
                Some(legacy) if !config.exists() && legacy.is_dir() => legacy,
                _ => config,
            },
            cache: base.cache_dir().join(APP),
            data: base.data_dir().join(APP),
            state: base
                .state_dir()
                .unwrap_or_else(|| base.data_dir())
                .join(APP),
        })
    }

    /// Everything under one directory.
    pub fn under(home: &Path) -> Self {
        Self {
            config: home.join("config"),
            cache: home.join("cache"),
            data: home.join("data"),
            state: home.join("state"),
        }
    }

    /// The explorer's saved settings.
    pub fn settings_file(&self) -> PathBuf {
        self.config.join("settings.toml")
    }

    /// Key binding overrides for the explorer.
    pub fn keymap_file(&self) -> PathBuf {
        self.config.join("keymap.toml")
    }

    /// Installed plugins.
    pub fn plugin_dir(&self) -> PathBuf {
        self.config.join("plugins")
    }
}

fn home_override() -> Option<PathBuf> {
    std::env::var_os("GRAVITYFILE_HOME")
        .filter(|home| !home.is_empty())
        .map(PathBuf::from)
}

/// Where versions before 0.4 kept settings and plugins on macOS.
fn legacy_config() -> Option<PathBuf> {
    if !cfg!(target_os = "macos") || home_override().is_some() {
        return None;
    }
    Some(home()?.join("Library/Application Support").join(APP))
}

/// Move the configuration directory of versions before 0.4 (macOS's
/// `~/Library/Application Support/gravityfile`) to the current location, if
/// only the old one exists. Returns the move made, if any. Until this runs,
/// [`Dirs::discover`] keeps using the old directory.
pub fn migrate_legacy_config() -> std::io::Result<Option<(PathBuf, PathBuf)>> {
    let (Some(legacy), Some(base)) = (legacy_config(), etcetera::choose_base_strategy().ok())
    else {
        return Ok(None);
    };
    let current = base.config_dir().join(APP);
    Ok(move_legacy(&legacy, &current)?.then_some((legacy, current)))
}

/// Rename `legacy` to `current` if only `legacy` exists.
fn move_legacy(legacy: &Path, current: &Path) -> std::io::Result<bool> {
    if current.exists() || !legacy.is_dir() {
        return Ok(false);
    }
    if let Some(parent) = current.parent() {
        std::fs::create_dir_all(parent)?;
    }
    match std::fs::rename(legacy, current) {
        Ok(()) => Ok(true),
        // Another process moved it first.
        Err(_) if current.exists() => Ok(false),
        Err(error) => Err(error),
    }
}

/// The user's home directory.
pub fn home() -> Option<PathBuf> {
    etcetera::home_dir().ok()
}

/// The user's configuration root (`~/.config`, `%APPDATA%`), where many tools
/// keep their settings.
pub fn config_root() -> Option<PathBuf> {
    etcetera::choose_base_strategy()
        .ok()
        .map(|base| base.config_dir())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_home_holds_every_directory() {
        let dirs = Dirs::under(Path::new("/portable"));
        assert_eq!(
            dirs.settings_file(),
            Path::new("/portable/config/settings.toml")
        );
        assert_eq!(dirs.plugin_dir(), Path::new("/portable/config/plugins"));
        assert_eq!(dirs.cache, Path::new("/portable/cache"));
    }

    #[test]
    fn a_legacy_directory_is_moved_once_and_never_over_a_current_one() {
        let temp = tempfile::tempdir().unwrap();
        let legacy = temp.path().join("Library/Application Support/gravityfile");
        let current = temp.path().join(".config/gravityfile");
        std::fs::create_dir_all(legacy.join("plugins")).unwrap();
        std::fs::write(legacy.join("settings.toml"), "theme = 'light'").unwrap();

        assert!(move_legacy(&legacy, &current).unwrap());
        assert!(!legacy.exists());
        assert_eq!(
            std::fs::read_to_string(current.join("settings.toml")).unwrap(),
            "theme = 'light'"
        );
        assert!(current.join("plugins").is_dir());

        // With both present, the current directory wins and nothing moves.
        std::fs::create_dir_all(&legacy).unwrap();
        assert!(!move_legacy(&legacy, &current).unwrap());
        assert!(legacy.exists());

        // Without a legacy directory, nothing is created.
        let fresh = temp.path().join("fresh/gravityfile");
        assert!(!move_legacy(&temp.path().join("missing"), &fresh).unwrap());
        assert!(!fresh.exists());
    }
}
