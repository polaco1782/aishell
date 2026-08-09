use std::ffi::OsString;

use anyhow::{Result, bail};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Shell {
    Bash,
    Zsh,
    Pwsh,
    Cmd,
}

impl Shell {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Bash => "bash",
            Self::Zsh => "zsh",
            Self::Pwsh => "powershell",
            Self::Cmd => "cmd",
        }
    }

    pub const fn generation_guidance(self) -> &'static str {
        match self {
            Self::Bash | Self::Zsh => "Do not mix syntax from another shell.",
            Self::Pwsh => {
                "Use PowerShell syntax supported by both Windows PowerShell 5.1 and PowerShell 7 unless the user requests a version-specific feature. Do not emit cmd.exe, Bash, or Zsh syntax."
            }
            Self::Cmd => {
                "Use cmd.exe built-ins and command syntax, not PowerShell, Bash, or Zsh syntax."
            }
        }
    }

    fn parse(value: &str) -> Result<Self> {
        match value {
            "bash" => Ok(Self::Bash),
            "zsh" => Ok(Self::Zsh),
            "powershell" => Ok(Self::Pwsh),
            "cmd" => Ok(Self::Cmd),
            _ => bail!("unsupported shell {value:?}; expected bash, zsh, powershell, or cmd"),
        }
    }

    fn parse_integration(value: &str) -> Result<Self> {
        let shell = Self::parse(value)?;
        if shell == Self::Cmd {
            bail!(
                "cmd.exe does not expose programmable edit-buffer integration; use `ai --shell cmd -- <description>`"
            );
        }
        Ok(shell)
    }
}

#[derive(Debug, Eq, PartialEq)]
pub enum PromptSource {
    Interactive,
    Argument(String),
    Stdin,
}

#[derive(Debug, Eq, PartialEq)]
pub enum Action {
    Generate {
        shell: Option<Shell>,
        prompt: PromptSource,
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
            return Ok(Action::Init(Shell::parse_integration(shell)?));
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

    let (shell, remaining) = match args.as_slice() {
        [flag, shell, rest @ ..] if flag == "--shell" => (Some(Shell::parse(shell)?), rest),
        _ => (None, args.as_slice()),
    };

    let prompt = if remaining == ["--stdin"] {
        PromptSource::Stdin
    } else {
        let remaining = match remaining {
            [separator, rest @ ..] if separator == "--" => rest,
            _ => remaining,
        };
        if remaining.is_empty() {
            return Ok(Action::Generate {
                shell,
                prompt: PromptSource::Interactive,
            });
        }

        let prompt = remaining.join(" ");
        if prompt.trim().is_empty() {
            bail!("the command description cannot be empty");
        }
        PromptSource::Argument(prompt)
    };

    Ok(Action::Generate { shell, prompt })
}

pub const HELP: &str = r#"Generate a shell command from a natural-language description.

Usage:
  ai [description...]
  ai [--shell bash|zsh|powershell|cmd] --stdin
  ai setup
  ai config path|show|check
  ai context path|show|clear
  ai init bash|zsh|powershell

Shell integration:
  source <(ai init bash)
  source <(ai init zsh)
  ai init powershell | Out-String | Invoke-Expression

Supported integration shells: Bash, Zsh, and PowerShell. cmd.exe can generate
commands with `--shell cmd`, but its built-in editor cannot provide Tab or
edit-buffer integration.
Supported AI providers: OpenRouter, OpenAI, llama.cpp, and vLLM.

Tab behavior after installing the integration:
  empty command line  open the `🤖 AI Prompt ›` prompt
  active AI prompt   submit the request, like Enter
  any other text     run normal shell completion

A generated command replaces the AI request in the same editable line but is
not executed. Review or edit it, then press Enter to execute it normally.
Clarifying questions and general answers are displayed without inserting an
executable line. The direct `ai [description...]` CLI prints its result because
only an installed shell integration can replace its parent shell's edit buffer.

Command risk:
  safe      light green; read-only with no meaningful side effects
  moderate  light yellow; bounded, normally recoverable changes
  high      light red; destructive, broad, secret-exposing, or difficult-to-reverse changes

Every generated command must have an exact risk classification. The color-coded
message appears immediately with no countdown. Setting `risk_warning = false`
in the `[safety]` config section hides the message but does not disable
classification or validation.

The integration keeps a bounded command-generation context in one private state
database. Use `ai context show` to inspect the current context or `clear` to
forget it.
"#;

#[cfg(test)]
mod tests {
    use super::{Action, PromptSource, Shell, parse};

    fn args(values: &[&str]) -> Vec<std::ffi::OsString> {
        values.iter().map(std::ffi::OsString::from).collect()
    }

    #[test]
    fn parses_a_plain_prompt() {
        assert_eq!(
            parse(args(&["run", "qemu", "with", "disk.vdi"])).unwrap(),
            Action::Generate {
                shell: None,
                prompt: PromptSource::Argument("run qemu with disk.vdi".into()),
            }
        );
    }

    #[test]
    fn parses_the_shell_widget_form() {
        assert_eq!(
            parse(args(&["--shell", "zsh", "--", "show files"])).unwrap(),
            Action::Generate {
                shell: Some(Shell::Zsh),
                prompt: PromptSource::Argument("show files".into()),
            }
        );
    }

    #[test]
    fn only_exact_management_commands_are_reserved() {
        assert_eq!(
            parse(args(&["setup", "a", "development", "environment"])).unwrap(),
            Action::Generate {
                shell: None,
                prompt: PromptSource::Argument("setup a development environment".into()),
            }
        );
    }

    #[test]
    fn requests_an_interactive_prompt_when_no_description_is_given() {
        assert_eq!(
            parse(args(&[])).unwrap(),
            Action::Generate {
                shell: None,
                prompt: PromptSource::Interactive,
            }
        );
        assert_eq!(
            parse(args(&["--shell", "bash"])).unwrap(),
            Action::Generate {
                shell: Some(Shell::Bash),
                prompt: PromptSource::Interactive,
            }
        );
    }

    #[test]
    fn parses_windows_shells_and_stdin_prompts() {
        assert_eq!(
            parse(args(&["init", "powershell"])).unwrap(),
            Action::Init(Shell::Pwsh)
        );
        assert_eq!(
            parse(args(&["--shell", "powershell", "--stdin"])).unwrap(),
            Action::Generate {
                shell: Some(Shell::Pwsh),
                prompt: PromptSource::Stdin,
            }
        );
        assert_eq!(
            parse(args(&["--shell", "cmd", "--", "show", "files"])).unwrap(),
            Action::Generate {
                shell: Some(Shell::Cmd),
                prompt: PromptSource::Argument("show files".into()),
            }
        );
        assert_eq!(
            parse(args(&["--", "--stdin"])).unwrap(),
            Action::Generate {
                shell: None,
                prompt: PromptSource::Argument("--stdin".into()),
            }
        );
    }

    #[test]
    fn cmd_does_not_claim_native_edit_buffer_integration() {
        let error = parse(args(&["init", "cmd"])).unwrap_err().to_string();
        assert!(error.contains("does not expose programmable edit-buffer integration"));
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
