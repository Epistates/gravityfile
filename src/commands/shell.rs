//! Shell integration, completions, and the manual page.

use std::io::Write;

use clap::CommandFactory;
use color_eyre::Result;

use crate::cli::{Cli, InitShell};

/// The `grav` wrapper function for `shell`.
pub fn init_script(shell: InitShell) -> &'static str {
    match shell {
        InitShell::Bash => include_str!("../shell/grav.bash"),
        InitShell::Zsh => include_str!("../shell/grav.zsh"),
        InitShell::Fish => include_str!("../shell/grav.fish"),
    }
}

pub fn init(shell: InitShell, output: &mut impl Write) -> Result<()> {
    output.write_all(init_script(shell).as_bytes())?;
    Ok(())
}

pub fn completions(shell: clap_complete::Shell, output: &mut impl Write) -> Result<()> {
    clap_complete::generate(shell, &mut Cli::command(), "grav", output);
    Ok(())
}

pub fn man(output: &mut impl Write) -> Result<()> {
    clap_mangen::Man::new(Cli::command()).render(output)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::ValueEnum;

    #[test]
    fn wrappers_invoke_the_binary_not_themselves() {
        for shell in InitShell::value_variants() {
            let script = init_script(*shell);
            assert!(script.contains("command grav --cwd-file="), "{shell:?}");
            assert!(!script.contains("gravityfile --cwd-file"), "{shell:?}");
        }
    }

    #[test]
    fn completions_and_manual_mention_every_command() {
        let mut output = Vec::new();
        completions(clap_complete::Shell::Zsh, &mut output).unwrap();
        let text = String::from_utf8(output).unwrap();
        for command in ["scan", "duplicates", "age", "export", "init"] {
            assert!(text.contains(command), "{command}");
        }
        assert!(!text.contains("__plugin-worker"));
        let mut output = Vec::new();
        man(&mut output).unwrap();
        let text = String::from_utf8(output).unwrap();
        assert!(text.starts_with(".ie"), "{}", &text[..40]);
        assert!(text.contains("grav\\-scan") || text.contains("scan"));
    }
}
