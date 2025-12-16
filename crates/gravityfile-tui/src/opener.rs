//! File opener functionality for opening files with configured applications.

use std::ffi::OsStr;
use std::path::Path;
use std::process::Command;

use crate::app::state::FileOpeners;

/// Known text/code file extensions that should be opened with an editor.
const TEXT_EXTENSIONS: &[&str] = &[
    // Programming languages
    "rs", "py", "js", "ts", "jsx", "tsx", "go", "c", "cpp", "h", "hpp",
    "java", "kt", "swift", "rb", "php", "cs", "fs", "scala", "clj",
    "hs", "ml", "elm", "ex", "exs", "erl", "lua", "r", "jl", "nim",
    "zig", "v", "d", "pas", "pl", "pm", "tcl", "awk", "sed",
    // Web
    "html", "htm", "css", "scss", "sass", "less", "vue", "svelte",
    // Config/Data
    "json", "yaml", "yml", "toml", "xml", "ini", "conf", "cfg",
    // Shell/Scripts
    "sh", "bash", "zsh", "fish", "ps1", "bat", "cmd",
    // Documentation
    "txt", "rst", "adoc", "org", "tex", "bib",
    // Other
    "sql", "graphql", "proto", "dockerfile", "makefile", "cmake",
    "gitignore", "gitattributes", "editorconfig",
];

/// Result of attempting to open a file.
pub enum OpenResult {
    /// File was opened successfully.
    Opened,
    /// Need to suspend terminal for TUI app.
    NeedsSuspend(Command),
    /// File type not supported or opener not found.
    #[allow(dead_code)]
    NotSupported(String),
    /// Error opening file.
    Error(String),
}

/// Determine how to open a file based on its extension and user settings.
pub fn open_file(
    path: &Path,
    openers: &FileOpeners,
    editor_config: &opensesame::EditorConfig,
) -> OpenResult {
    let extension = path
        .extension()
        .and_then(OsStr::to_str)
        .map(|s| s.to_lowercase());

    match extension.as_deref() {
        // Markdown files
        Some("md" | "markdown") => open_markdown(path, openers),

        // Text/code files - use editor
        Some(ext) if is_text_extension(ext) => open_with_editor(path, editor_config),

        // Other files - use system default
        _ => open_with_system(path, openers),
    }
}

/// Check if an extension is a known text/code file type.
fn is_text_extension(ext: &str) -> bool {
    TEXT_EXTENSIONS.contains(&ext)
}

/// Open a markdown file.
fn open_markdown(path: &Path, openers: &FileOpeners) -> OpenResult {
    let opener = &openers.md;

    // Check for treemd feature
    #[cfg(feature = "treemd")]
    {
        if opener == "treemd" {
            // Use built-in treemd - needs terminal suspend
            let mut cmd = Command::new("treemd");
            cmd.arg(path);
            return OpenResult::NeedsSuspend(cmd);
        }
    }

    // External treemd or other opener
    if opener == "treemd" {
        // Check if treemd is available
        if which_exists("treemd") {
            let mut cmd = Command::new("treemd");
            cmd.arg(path);
            return OpenResult::NeedsSuspend(cmd);
        } else {
            // Fallback to system open
            return open_with_system(path, openers);
        }
    }

    // Custom opener
    if opener == "editor" {
        return open_with_editor(path, &opensesame::EditorConfig::default());
    }

    if opener == "open" || opener == "system" {
        return open_with_system(path, openers);
    }

    // Custom command
    let mut cmd = Command::new(opener);
    cmd.arg(path);
    OpenResult::NeedsSuspend(cmd)
}

/// Open a file with the configured editor.
///
/// Resolution order:
/// 1. User config editor (from settings)
/// 2. $VISUAL environment variable
/// 3. $EDITOR environment variable
/// 4. Fallback to vim
fn open_with_editor(path: &Path, config: &opensesame::EditorConfig) -> OpenResult {
    // Determine which editor to use
    let editor = config
        .editor
        .clone()
        .or_else(|| std::env::var("VISUAL").ok())
        .or_else(|| std::env::var("EDITOR").ok())
        .unwrap_or_else(|| "vim".to_string());

    // Build command
    let mut cmd = Command::new(&editor);
    cmd.arg(path);

    // Inherit stdio for terminal editors
    cmd.stdin(std::process::Stdio::inherit())
        .stdout(std::process::Stdio::inherit())
        .stderr(std::process::Stdio::inherit());

    OpenResult::NeedsSuspend(cmd)
}

/// Open a file with the system default application.
fn open_with_system(path: &Path, openers: &FileOpeners) -> OpenResult {
    let opener = &openers.default;

    if opener == "open" || opener == "system" {
        // Use the `open` crate for cross-platform system open
        match open::that(path) {
            Ok(()) => OpenResult::Opened,
            Err(e) => OpenResult::Error(format!("Failed to open with system: {e}")),
        }
    } else {
        // Custom command
        let mut cmd = Command::new(opener);
        cmd.arg(path);
        OpenResult::NeedsSuspend(cmd)
    }
}

/// Check if a command exists in PATH.
fn which_exists(cmd: &str) -> bool {
    std::process::Command::new("which")
        .arg(cmd)
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}
