mod cli;
mod config;
mod openrouter;
mod shell;

use std::env;
use std::io::{self, Write};
use std::path::Path;

use anyhow::{Context, Result, bail};

use crate::cli::{Action, Shell};
use crate::config::Config;
use crate::openrouter::{GeneratedOutput, OpenRouterClient};

fn main() {
    if let Err(error) = run() {
        eprintln!("ai: {error:#}");
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    match cli::parse(env::args_os().skip(1))? {
        Action::Generate { shell, prompt } => {
            let prompt = prompt.map_or_else(read_interactive_prompt, Ok)?;
            generate(shell.unwrap_or_else(detect_shell), &prompt)
        }
        Action::Setup => {
            let path = config::interactive_setup()?;
            println!("Configuration saved to {}", path.display());
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
        Action::Init(shell) => {
            print!("{}", shell::init_script(shell));
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
    eprint!("[ai] What should the shell command do? ");
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
    Ok(prompt.to_owned())
}

fn generate(shell: Shell, prompt: &str) -> Result<()> {
    let config = Config::load()?;
    let client = OpenRouterClient::new(&config)?;
    match client.generate(prompt, shell)? {
        GeneratedOutput::Command(command) => println!("{command}"),
        // Clarifications are informational, so keep stdout empty for shell widgets.
        GeneratedOutput::Clarification(question) => eprintln!("[ai] {question}"),
    }
    Ok(())
}

fn check_config() -> Result<()> {
    let config = Config::load()?;
    let client = OpenRouterClient::new(&config)?;
    client.check_key()?;
    println!(
        "Configuration is valid; OpenRouter authenticated successfully with model {}.",
        config.openrouter.model
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
        // Bash is the conservative default for direct invocations outside a widget.
        .unwrap_or(Shell::Bash)
}
