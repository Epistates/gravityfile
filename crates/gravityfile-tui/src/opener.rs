//! File opener functionality for opening files with configured applications.

use std::ffi::OsStr;
use std::path::Path;
use std::process::Command;

use crate::app::state::FileOpeners;

/// Known text/code file extensions that should be opened with an editor.
const TEXT_EXTENSIONS: &[&str] = &[
    // Programming languages
    "rs",
    "py",
    "js",
    "ts",
    "jsx",
    "tsx",
    "go",
    "c",
    "cpp",
    "h",
    "hpp",
    "java",
    "kt",
    "swift",
    "rb",
    "php",
    "cs",
    "fs",
    "scala",
    "clj",
    "hs",
    "ml",
    "elm",
    "ex",
    "exs",
    "erl",
    "lua",
    "r",
    "jl",
    "nim",
    "zig",
    "v",
    "d",
    "pas",
    "pl",
    "pm",
    "tcl",
    "awk",
    "sed",
    // Web
    "html",
    "htm",
    "css",
    "scss",
    "sass",
    "less",
    "vue",
    "svelte",
    // Config/Data
    "json",
    "yaml",
    "yml",
    "toml",
    "xml",
    "ini",
    "conf",
    "cfg",
    // Shell/Scripts
    "sh",
    "bash",
    "zsh",
    "fish",
    "ps1",
    "bat",
    "cmd",
    // Documentation
    "txt",
    "rst",
    "adoc",
    "org",
    "tex",
    "bib",
    // Other
    "sql",
    "graphql",
    "proto",
    "dockerfile",
    "makefile",
    "cmake",
    "gitignore",
    "gitattributes",
    "editorconfig",
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
    let path = match std::path::absolute(path) {
        Ok(path) => path,
        Err(error) => return OpenResult::Error(format!("Cannot resolve file path: {error}")),
    };
    let path = path.as_path();
    let extension = path
        .extension()
        .and_then(OsStr::to_str)
        .map(|s| s.to_lowercase());

    match extension.as_deref() {
        // Markdown files
        Some("md" | "markdown") => open_markdown(path, openers, editor_config),

        // Text/code files - use editor
        Some(ext) if is_text_extension(ext) => {
            if openers.text == "editor" {
                open_with_editor(path, editor_config)
            } else {
                let mut cmd = Command::new(&openers.text);
                cmd.arg(path);
                OpenResult::NeedsSuspend(cmd)
            }
        }

        // Other files - use system default
        _ => open_with_system(path, openers),
    }
}

/// Check if an extension is a known text/code file type.
fn is_text_extension(ext: &str) -> bool {
    TEXT_EXTENSIONS.contains(&ext)
}

/// Open a markdown file.
fn open_markdown(
    path: &Path,
    openers: &FileOpeners,
    config: &opensesame::EditorConfig,
) -> OpenResult {
    let opener = &openers.md;

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
        return open_with_editor(path, config);
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
/// 1. User config executable or editor kind (from settings)
/// 2. $VISUAL environment variable
/// 3. $EDITOR environment variable
/// 4. Fallback to vim
fn open_with_editor(path: &Path, config: &opensesame::EditorConfig) -> OpenResult {
    match editor_command(path, config) {
        Ok(command) => OpenResult::NeedsSuspend(command),
        Err(error) => OpenResult::Error(format!("Cannot configure editor: {error}")),
    }
}

pub(crate) fn editor_command(
    path: &Path,
    config: &opensesame::EditorConfig,
) -> std::io::Result<Command> {
    build_editor_command(
        path,
        config,
        std::env::var_os("VISUAL"),
        std::env::var_os("EDITOR"),
    )
}

fn build_editor_command(
    path: &Path,
    config: &opensesame::EditorConfig,
    visual: Option<std::ffi::OsString>,
    editor: Option<std::ffi::OsString>,
) -> std::io::Result<Command> {
    let program = config
        .editor
        .as_ref()
        .map(std::ffi::OsString::from)
        .or_else(|| {
            config
                .editor_kind
                .as_ref()
                .map(|kind| kind.0.default_binary().into())
        })
        .or_else(|| visual.filter(|value| !value.is_empty()))
        .or_else(|| editor.filter(|value| !value.is_empty()))
        .unwrap_or_else(|| "vim".into());
    if program.is_empty() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "configured editor is empty",
        ));
    }
    // Treat the program as an executable name/path, never as shell code. An
    // absolute file operand cannot be mistaken for an option starting with '-'.
    let mut command = Command::new(program);
    command.args(&config.args).arg(std::path::absolute(path)?);
    command
        .stdin(std::process::Stdio::inherit())
        .stdout(std::process::Stdio::inherit())
        .stderr(std::process::Stdio::inherit());
    Ok(command)
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

/// Check if a command exists in PATH using the `which` crate (no subprocess).
fn which_exists(cmd: &str) -> bool {
    which::which(cmd).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsString;

    #[test]
    fn editor_settings_and_arguments_take_precedence_without_shell_parsing() {
        let config = opensesame::EditorConfig {
            editor: Some("/editor with spaces".into()),
            args: vec!["--wait".into(), "literal $(touch marker)".into()],
            ..Default::default()
        };
        let command = build_editor_command(
            Path::new("-option.txt"),
            &config,
            Some("visual".into()),
            Some("editor".into()),
        )
        .unwrap();
        assert_eq!(command.get_program(), "/editor with spaces");
        let arguments: Vec<_> = command.get_args().collect();
        assert_eq!(arguments[0], "--wait");
        assert_eq!(arguments[1], "literal $(touch marker)");
        assert_eq!(arguments[2], std::path::absolute("-option.txt").unwrap());
    }

    #[test]
    fn editor_environment_order_and_empty_values_are_consistent() {
        let config = opensesame::EditorConfig::default();
        for (visual, editor, expected) in [
            (Some("visual"), Some("editor"), "visual"),
            (Some(""), Some("editor"), "editor"),
            (None, Some(""), "vim"),
        ] {
            let command = build_editor_command(
                Path::new("file"),
                &config,
                visual.map(OsString::from),
                editor.map(OsString::from),
            )
            .unwrap();
            assert_eq!(command.get_program(), expected);
        }
        let config = opensesame::EditorConfig {
            editor: Some(String::new()),
            ..Default::default()
        };
        assert!(build_editor_command(Path::new("file"), &config, None, None).is_err());
    }

    #[test]
    fn markdown_honors_editor_config_and_text_honors_custom_opener() {
        let openers = FileOpeners {
            md: "editor".into(),
            text: "custom-text".into(),
            ..Default::default()
        };
        let config = opensesame::EditorConfig::with_editor("configured");
        for (file, expected) in [("note.md", "configured"), ("code.rs", "custom-text")] {
            let OpenResult::NeedsSuspend(command) = open_file(Path::new(file), &openers, &config)
            else {
                panic!("expected editor command")
            };
            assert_eq!(command.get_program(), expected);
            assert_eq!(
                command.get_args().last().unwrap(),
                std::path::absolute(file).unwrap()
            );
        }
    }
}
