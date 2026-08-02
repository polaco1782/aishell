use std::ffi::OsString;

use anyhow::{Result, bail};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Shell {
    Bash,
    Zsh,
}

impl Shell {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Bash => "bash",
            Self::Zsh => "zsh",
        }
    }

    fn parse(value: &str) -> Result<Self> {
        match value {
            "bash" => Ok(Self::Bash),
            "zsh" => Ok(Self::Zsh),
            _ => bail!("unsupported shell {value:?}; expected bash or zsh"),
        }
    }
}

#[derive(Debug, Eq, PartialEq)]
pub enum Action {
    Generate {
        shell: Option<Shell>,
        prompt: Option<String>,
    },
    Setup,
    ConfigPath,
    ConfigShow,
    ConfigCheck,
    ContextPath,
    ContextShow,
    ContextClear,
    Init(Shell),
    Help,
    Version,
}

pub fn parse(args: impl IntoIterator<Item = OsString>) -> Result<Action> {
    let args = args
        .into_iter()
        .map(|argument| {
            argument
                .into_string()
                .map_err(|_| anyhow::anyhow!("arguments must be valid UTF-8"))
        })
        .collect::<Result<Vec<_>>>()?;

    match args.as_slice() {
        [argument] if argument == "--help" || argument == "-h" => return Ok(Action::Help),
        [argument] if argument == "--version" || argument == "-V" => {
            return Ok(Action::Version);
        }
        [command] if command == "setup" => return Ok(Action::Setup),
        [command, shell] if command == "init" => {
            return Ok(Action::Init(Shell::parse(shell)?));
        }
        [command, action] if command == "config" && action == "path" => {
            return Ok(Action::ConfigPath);
        }
        [command, action] if command == "config" && action == "show" => {
            return Ok(Action::ConfigShow);
        }
        [command, action] if command == "config" && action == "check" => {
            return Ok(Action::ConfigCheck);
        }
        [command, action] if command == "context" && action == "path" => {
            return Ok(Action::ContextPath);
        }
        [command, action] if command == "context" && action == "show" => {
            return Ok(Action::ContextShow);
        }
        [command, action] if command == "context" && action == "clear" => {
            return Ok(Action::ContextClear);
        }
        _ => {}
    }

    let (shell, prompt_start) = match args.as_slice() {
        [flag, shell, rest @ ..] if flag == "--shell" => {
            let offset = usize::from(rest.first().is_some_and(|argument| argument == "--"));
            (Some(Shell::parse(shell)?), 2 + offset)
        }
        [separator, ..] if separator == "--" => (None, 1),
        _ => (None, 0),
    };

    let prompt = if args[prompt_start..].is_empty() {
        None
    } else {
        let prompt = args[prompt_start..].join(" ");
        if prompt.trim().is_empty() {
            bail!("the command description cannot be empty");
        }
        Some(prompt)
    };

    Ok(Action::Generate { shell, prompt })
}

pub const HELP: &str = r#"Generate a shell command from a natural-language description.

Usage:
  ai [description...]
  ai setup
  ai config path|show|check
  ai context path|show|clear
  ai init bash|zsh

Shell integration:
  source <(ai init bash)
  source <(ai init zsh)

After installing the integration, press Tab on an empty command line to open an
`AI Command>` prompt. Enter a natural-language description there; the generated
command replaces the editable command line but is not executed. Tab always uses
normal shell completion when the command line already contains text. General
questions receive a printed answer instead of a command.

The integration keeps a bounded command-generation context in one private state
database. Use `ai context show` to inspect the current context or `clear` to
forget it.
"#;

#[cfg(test)]
mod tests {
    use super::{Action, Shell, parse};

    fn args(values: &[&str]) -> Vec<std::ffi::OsString> {
        values.iter().map(std::ffi::OsString::from).collect()
    }

    #[test]
    fn parses_a_plain_prompt() {
        assert_eq!(
            parse(args(&["run", "qemu", "with", "disk.vdi"])).unwrap(),
            Action::Generate {
                shell: None,
                prompt: Some("run qemu with disk.vdi".into()),
            }
        );
    }

    #[test]
    fn parses_the_shell_widget_form() {
        assert_eq!(
            parse(args(&["--shell", "zsh", "--", "show files"])).unwrap(),
            Action::Generate {
                shell: Some(Shell::Zsh),
                prompt: Some("show files".into()),
            }
        );
    }

    #[test]
    fn only_exact_management_commands_are_reserved() {
        assert_eq!(
            parse(args(&["setup", "a", "development", "environment"])).unwrap(),
            Action::Generate {
                shell: None,
                prompt: Some("setup a development environment".into()),
            }
        );
    }

    #[test]
    fn requests_an_interactive_prompt_when_no_description_is_given() {
        assert_eq!(
            parse(args(&[])).unwrap(),
            Action::Generate {
                shell: None,
                prompt: None,
            }
        );
        assert_eq!(
            parse(args(&["--shell", "bash"])).unwrap(),
            Action::Generate {
                shell: Some(Shell::Bash),
                prompt: None,
            }
        );
    }

    #[test]
    fn parses_context_management_commands() {
        assert_eq!(
            parse(args(&["context", "path"])).unwrap(),
            Action::ContextPath
        );
        assert_eq!(
            parse(args(&["context", "show"])).unwrap(),
            Action::ContextShow
        );
        assert_eq!(
            parse(args(&["context", "clear"])).unwrap(),
            Action::ContextClear
        );
    }
}
