# aishell

`aishell` turns a natural-language description into a shell command directly
from the interactive command line:

```console
$ <Tab>
AI Command> run qemu with disk image xyz.vdi
$ qemu-system-x86_64 -drive file=xyz.vdi,format=vdi
```

The generated command replaces the current Bash or Zsh edit buffer. It is never
executed automatically, so it can be reviewed and edited before pressing Enter.

## Build and configure

Building requires Rust 1.95 or newer.

```sh
cargo build --release
cargo install --path .
ai setup
```

`ai setup` asks for an OpenRouter API key without echoing it and writes the
configuration to `~/.config/aishell/config.toml`. If `XDG_CONFIG_HOME` is an
absolute path, it is used instead. The directory and file are created with modes
`0700` and `0600` respectively.

An example configuration is:

```toml
[openrouter]
api_key = "sk-or-v1-..."
model = "openrouter/auto"
base_url = "https://openrouter.ai/api/v1"

[generation]
timeout_seconds = 20
max_output_tokens = 256

[context]
enabled = true
max_turns = 6
```

Useful configuration commands:

```sh
ai config path
ai config show     # API key is redacted
ai config check    # validates the key with OpenRouter
ai context path    # prints the central history database path
ai context show    # shows the current bounded context
ai context clear   # forgets the current context
```

## Shell integration

Add the matching line to `.bashrc` or `.zshrc`:

```sh
source <(ai init bash)
source <(ai init zsh)
```

Press Tab on a completely empty command line to switch to the `AI Command>`
prompt. Type the natural-language request and press Enter. When generation
finishes, the result is placed in the editable command line as though you had
typed it, but is not executed.

If the command line contains any text, Tab always performs normal shell
completion. There is no command-or-natural-language classifier. General
questions such as `what can you do?` receive a conversational response below the
prompt. If a requested operation needs more detail, `ai` asks a clarifying
question. Answers and questions leave the command line empty; only generated
commands are inserted. If generation fails, the error is displayed and the
command line remains empty.

The integration binds only Tab in the Bash and Zsh Emacs and vi-insert keymaps.
A shell setup that assigns custom Tab behavior should load the `ai` integration
after its completion framework.

Running `ai` without the shell integration prints the generated command to
standard output:

```sh
ai list TCP listeners with process names
```

If no description is passed, `ai` asks for one interactively before generating
the command.

## Command context

By default, `aishell` sends the previous six requests and generated results with
the next request. This lets follow-ups such as `create a filesystem on it` reuse
the concrete filename selected by an earlier command. The current working
directory is included on every turn.

History is kept in one private SQLite database under
`$XDG_STATE_HOME/aishell/history.sqlite3`, or
`~/.local/state/aishell/history.sqlite3` when `XDG_STATE_HOME` is not an
absolute path. The directory and database use modes `0700` and `0600`; no
`.aishell` file or other history is written into a project. Context is separated
by interactive shell session and Git working tree (or the exact directory when
outside Git). Direct invocations without the shell integration use the working
tree or directory as their context.

Saved commands are explicitly treated as unconfirmed: `aishell` knows what it
inserted for review, but cannot assume the user executed it unchanged. Set
`context.enabled = false` to disable history, reduce `context.max_turns` to send
less history, or use `ai context clear` to erase the current context.

Use `ai -- <description>` when a description exactly matches one of the
management commands shown above.

No environment variable is required for the API key, model, or endpoint.
