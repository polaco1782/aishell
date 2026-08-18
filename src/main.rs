mod cli;
mod config;
mod context;
mod file_tools;
mod paths;
mod provider;
mod secure_fs;
mod shell;
mod system_info;
mod ui;
#[cfg(windows)]
mod windows_console;

use std::env;
use std::io::{self, Read, Write};
use std::path::Path;

use anyhow::{Context, Result, bail};

use crate::cli::{Action, PromptSource, Shell};
use crate::config::Config;
use crate::context::{ContextResponse, ContextStore};
use crate::file_tools::FileIoOutcome;
use crate::provider::{AiClient, DestructiveRisk, GeneratedOutput};
use crate::system_info::SystemInfo;

fn main() {
    if let Err(error) = run() {
        eprintln!("{} ai · {error:#}", ui::ERROR);
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    match cli::parse(env::args_os().skip(1))? {
        Action::Generate { shell, prompt } => {
            let prompt = match prompt {
                PromptSource::Interactive => read_interactive_prompt()?,
                PromptSource::Argument(prompt) => prompt,
                PromptSource::Stdin => read_stdin_prompt()?,
            };
            generate(shell.unwrap_or_else(detect_shell), &prompt)
        }
        Action::Setup => {
            let path = config::interactive_setup()?;
            println!("{} Configuration saved · {}", ui::SUCCESS, path.display());
            Ok(())
        }
        Action::ConfigPath => {
            println!("{}", Config::path()?.display());
            Ok(())
        }
        Action::ConfigShow => {
            print!("{}", Config::load()?.redacted_toml());
            Ok(())
        }
        Action::ConfigCheck => check_config(),
        Action::ContextPath => {
            println!("{}", ContextStore::path()?.display());
            Ok(())
        }
        Action::ContextShow => show_context(),
        Action::ContextClear => clear_context(),
        Action::Logs => show_logs(),
        Action::Init(shell) => {
            print!("{}", shell::init_script(shell)?);
            Ok(())
        }
        Action::Help => {
            print!("{}", cli::HELP);
            Ok(())
        }
        Action::Version => {
            println!("ai {}", env!("CARGO_PKG_VERSION"));
            Ok(())
        }
    }
}

fn read_interactive_prompt() -> Result<String> {
    // Keep the question on stderr so stdout remains only the generated command,
    // which lets a shell widget safely capture and insert the result.
    eprint!("{}", ui::AI_PROMPT);
    io::stderr()
        .flush()
        .context("could not display the command prompt")?;

    let mut prompt = String::new();
    if io::stdin()
        .read_line(&mut prompt)
        .context("could not read the command description")?
        == 0
    {
        bail!("no command description was provided");
    }

    let prompt = prompt.trim();
    if prompt.is_empty() {
        bail!("the command description cannot be empty");
    }
    eprintln!("{}", ui::THINKING);
    Ok(prompt.to_owned())
}

fn read_stdin_prompt() -> Result<String> {
    const MAX_PROMPT_BYTES: u64 = 16 * 1024;

    let mut bytes = Vec::new();
    io::stdin()
        .take(MAX_PROMPT_BYTES + 1)
        .read_to_end(&mut bytes)
        .context("could not read the command description from stdin")?;
    if bytes.len() as u64 > MAX_PROMPT_BYTES {
        bail!("the command description from stdin exceeds {MAX_PROMPT_BYTES} bytes");
    }

    let prompt = String::from_utf8(bytes)
        .context("the command description from stdin must be valid UTF-8")?;
    let prompt = prompt.trim();
    if prompt.is_empty() {
        bail!("the command description from stdin cannot be empty");
    }
    Ok(prompt.to_owned())
}

fn generate(shell: Shell, prompt: &str) -> Result<()> {
    let config = Config::load()?;
    let client = AiClient::new(&config)?;
    let needs_database = config.context.enabled || config.tools.file_io;
    let mut context = if needs_database {
        match ContextStore::open(config.context.max_turns) {
            Ok(store) => Some(store),
            Err(error) if config.tools.file_io => {
                return Err(error).context("file I/O auditing is unavailable");
            }
            Err(error) => {
                eprintln!("{} Context is unavailable · {error:#}", ui::WARNING);
                None
            }
        }
    } else {
        None
    };
    let history = if config.context.enabled {
        context.as_ref().map_or_else(Vec::new, |store| {
            store.load().unwrap_or_else(|error| {
                eprintln!("{} Context could not be loaded · {error:#}", ui::WARNING);
                Vec::new()
            })
        })
    } else {
        Vec::new()
    };
    let working_directory = context
        .as_ref()
        .map(|store| store.working_directory().into())
        .or_else(|| env::current_dir().ok())
        .unwrap_or_else(|| "<unknown>".into());
    let system_info = SystemInfo::detect();
    let output = client.generate(
        prompt,
        shell,
        &history,
        &working_directory,
        &system_info,
        |event| {
            context
                .as_ref()
                .ok_or_else(|| "file I/O auditing is unavailable".to_owned())
                .and_then(|store| {
                    store
                        .append_file_io(prompt, event)
                        .map_err(|error| format!("{error:#}"))
                })
        },
    )?;

    if config.context.enabled
        && let Some(store) = context.as_mut()
    {
        let response = match &output {
            GeneratedOutput::Command { command, .. } => ContextResponse::Command(command.clone()),
            GeneratedOutput::Clarification(question) => {
                ContextResponse::Clarification(question.clone())
            }
            GeneratedOutput::Answer(answer) => ContextResponse::Answer(answer.clone()),
        };
        if let Err(error) = store.append(prompt, &response) {
            eprintln!("{} Context could not be saved · {error:#}", ui::WARNING);
        }
    }

    match output {
        GeneratedOutput::Command { command, risk } => {
            if config.safety.risk_warning {
                warn_before_command(risk)?;
            }
            println!("{command}");
        }
        // Non-command responses stay off stdout so shell widgets never insert them.
        GeneratedOutput::Clarification(question) => eprintln!("{} {question}", ui::AI),
        GeneratedOutput::Answer(answer) => eprintln!("{} {answer}", ui::AI),
    }
    Ok(())
}

fn warn_before_command(risk: DestructiveRisk) -> Result<()> {
    warn_before_command_with(risk, &mut io::stderr().lock())
}

fn warn_before_command_with(risk: DestructiveRisk, stderr: &mut impl Write) -> Result<()> {
    let consequence = match risk {
        DestructiveRisk::Safe => "is unlikely to cause damage, but review it carefully.",
        DestructiveRisk::Moderate => "may modify your system or data. Review it carefully.",
        DestructiveRisk::High => {
            "may cause destructive or difficult-to-reverse changes. Review it carefully."
        }
    };
    writeln!(
        stderr,
        "{}{}  · Command {consequence} {}",
        risk.message_color(),
        ui::WARNING,
        ui::RESET_COLOR
    )
    .context("could not display the command risk warning")
}

fn show_context() -> Result<()> {
    let config = Config::load()?;
    let store = ContextStore::open(config.context.max_turns)?;
    let turns = store.load()?;
    println!("{} Context · {}", ui::CONTEXT, store.scope_description());
    println!("{} Database · {}", ui::DATABASE, store.path_ref().display());
    if turns.is_empty() {
        println!("{} No saved turns yet.", ui::EMPTY);
        return Ok(());
    }

    for (index, turn) in turns.iter().enumerate() {
        println!(
            "\n─ {} · {} · {}",
            index + 1,
            turn.created_at,
            turn.working_directory
        );
        println!("{} You · {}", ui::REQUEST, turn.request);
        match &turn.response {
            ContextResponse::Command(command) => {
                println!(
                    "{} Command · {command}  (execution unconfirmed)",
                    ui::COMMAND
                );
            }
            ContextResponse::Clarification(question) => {
                println!("{} Question · {question}", ui::AI);
            }
            ContextResponse::Answer(answer) => {
                println!("{} Answer · {answer}", ui::AI);
            }
        }
    }
    Ok(())
}

fn clear_context() -> Result<()> {
    let config = Config::load()?;
    let mut store = ContextStore::open(config.context.max_turns)?;
    if store.clear()? {
        println!(
            "{} Context cleared · {}",
            ui::SUCCESS,
            store.scope_description()
        );
    } else {
        println!(
            "{} Nothing to clear · {}",
            ui::EMPTY,
            store.scope_description()
        );
    }
    Ok(())
}

fn show_logs() -> Result<()> {
    let config = Config::load()?;
    let store = ContextStore::open(config.context.max_turns)?;
    let entries = store.load_file_io_log()?;
    println!(
        "{} File I/O log · {}",
        ui::CONTEXT,
        store.scope_description()
    );
    println!("{} Database · {}", ui::DATABASE, store.path_ref().display());
    if entries.is_empty() {
        println!("{} No file I/O logged yet.", ui::EMPTY);
        return Ok(());
    }

    for (index, entry) in entries.iter().enumerate() {
        let result = match &entry.event.outcome {
            FileIoOutcome::Succeeded { .. } => "succeeded",
            FileIoOutcome::Failed { .. } => "failed",
        };
        println!(
            "\n─ {} · {} · {} · {result}",
            index + 1,
            entry.created_at,
            entry.event.operation.as_str()
        );
        println!(
            "{} You · {}",
            ui::REQUEST,
            terminal_safe_line(&entry.request)
        );
        println!(
            "  Directory · {}",
            terminal_safe_line(&entry.working_directory)
        );
        println!(
            "  File · {}",
            entry
                .event
                .path
                .as_deref()
                .map(terminal_safe_line)
                .unwrap_or_else(|| "<unavailable>".into())
        );
        if let Some(offset) = entry.event.offset {
            println!("  Offset · {offset}");
        }
        match &entry.event.outcome {
            FileIoOutcome::Succeeded { content } => {
                println!("  Content · {} bytes", content.len());
                println!("{}", terminal_safe_content(content));
            }
            FileIoOutcome::Failed { reason } => {
                println!("  Reason · {}", terminal_safe_line(reason));
            }
        }
    }
    Ok(())
}

fn terminal_safe_content(content: &str) -> String {
    terminal_safe_text(content, true)
}

fn terminal_safe_line(content: &str) -> String {
    terminal_safe_text(content, false)
}

fn terminal_safe_text(content: &str, preserve_layout: bool) -> String {
    let mut safe = String::with_capacity(content.len());
    for character in content.chars() {
        if !character.is_control() || (preserve_layout && (character == '\n' || character == '\t'))
        {
            safe.push(character);
        } else {
            safe.extend(character.escape_default());
        }
    }
    safe
}

fn check_config() -> Result<()> {
    let config = Config::load()?;
    let client = AiClient::new(&config)?;
    client.check()?;
    println!(
        "{} {} ready · {}",
        ui::SUCCESS,
        config.provider.kind,
        config.provider.model
    );
    Ok(())
}

fn detect_shell() -> Shell {
    env::var_os("SHELL")
        .as_deref()
        .and_then(|path| Path::new(path).file_name())
        .and_then(|name| name.to_str())
        .and_then(|name| match name {
            "zsh" => Some(Shell::Zsh),
            "bash" => Some(Shell::Bash),
            _ => None,
        })
        .unwrap_or(DEFAULT_SHELL)
}

#[cfg(windows)]
const DEFAULT_SHELL: Shell = Shell::Pwsh;

#[cfg(not(windows))]
const DEFAULT_SHELL: Shell = Shell::Bash;

#[cfg(test)]
mod tests {
    use super::{
        DestructiveRisk, terminal_safe_content, terminal_safe_line, warn_before_command_with,
    };

    #[test]
    fn risk_messages_use_their_assigned_light_color_without_a_countdown() {
        for risk in [
            DestructiveRisk::Safe,
            DestructiveRisk::Moderate,
            DestructiveRisk::High,
        ] {
            let mut warning = Vec::new();
            warn_before_command_with(risk, &mut warning).unwrap();

            let warning = String::from_utf8(warning).unwrap();
            assert!(warning.starts_with(risk.message_color()));
            assert!(warning.ends_with("\x1b[0m\n"));
            assert!(!warning.contains("Command available in"));
        }
    }

    #[test]
    fn logged_content_cannot_emit_terminal_control_sequences() {
        assert_eq!(
            terminal_safe_content("first\n\x1b[31msecond\r\n"),
            "first\n\\u{1b}[31msecond\\r\n"
        );
        assert_eq!(terminal_safe_line("denied\nreason"), "denied\\nreason");
    }
}
