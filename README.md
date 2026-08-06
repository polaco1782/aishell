# AIshell

`aishell` turns a natural-language description into a shell command directly
from the interactive command line. The generated command remains editable and
is never executed automatically. It can remember the context so it is possible
to do multiple interactions with the resulting commands.

> [!NOTE]
> Interactive shell integration is currently available for **Bash and Zsh
> only**. Other shells can run the standalone `ai` command, but they do not yet
> have the Tab shortcut or in-place command-line replacement.
>
> Supported AI providers are **OpenRouter**, **OpenAI**, **llama.cpp**, and
> **vLLM**. llama.cpp and vLLM use their OpenAI-compatible Chat Completions
> servers.

## Demo

![Tab opens the aishell prompt, submits a natural-language request, and replaces it with an editable command](aishell-demo.gif)

Press Tab on an empty command line, describe the operation, and press Tab again.
The AI request is replaced in place by a command that can be reviewed before
pressing Enter.

## Build and configure

Building requires Rust 1.95 or newer.

The provider, model, endpoint, and optional credentials are configured by
`ai setup`; provider credentials are read from the private configuration file,
not environment variables.

```sh
cargo build --release
cargo install --path .
ai setup
```

`ai setup` interactively selects OpenRouter, OpenAI, llama.cpp, or vLLM, then
asks for that provider's model and API base URL. API keys are required for
OpenRouter and OpenAI and optional for authenticated llama.cpp or vLLM servers;
key input is never echoed. The configuration is written to
`~/.config/aishell/config.toml`. If `XDG_CONFIG_HOME` is an absolute path, it is
used instead. The directory and file are created with modes `0700` and `0600`
respectively.

The provider configuration schema is intentionally current-only. After updating
from an OpenRouter-only version, run `ai setup` to replace the old configuration.
An OpenAI example is:

```toml
[provider]
type = "openai"
api_key = "sk-..."
model = "gpt-5.6-luna"
base_url = "https://api.openai.com/v1"

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
ai config check    # checks authentication, endpoint, and configured model
ai context path    # prints the central history database path
ai context show    # shows the current bounded context
ai context clear   # forgets the current context
```

Provider defaults used by `ai setup`:

| Provider | API base URL | Model/key behavior |
| --- | --- | --- |
| OpenRouter | `https://openrouter.ai/api/v1` | Defaults to `openrouter/auto`; API key required. |
| OpenAI | `https://api.openai.com/v1` | Defaults to `gpt-5.6-luna`; API key required. |
| llama.cpp | `http://127.0.0.1:8080/v1` | Served model is prompted; API key optional. |
| vLLM | `http://127.0.0.1:8000/v1` | Served model is prompted; API key optional. |

Plain HTTP endpoints are accepted only on loopback addresses. Remote provider
URLs must use HTTPS. `ai config check` uses OpenRouter's key endpoint, OpenAI's
authenticated model endpoint, or the OpenAI-compatible model catalog for
llama.cpp and vLLM.

## Shell integration

The edit-buffer integration currently supports these shells:

| Shell | Integration | Configuration |
| --- | --- | --- |
| Bash | Readline | Add `source <(ai init bash)` to `.bashrc`. |
| Zsh | ZLE | Add `source <(ai init zsh)` to `.zshrc`. |

No Fish, Nushell, or other shell integration is provided yet. Those shells can
still use `ai [description...]` as a normal CLI command, which prints the
generated command instead of inserting it into the current edit buffer.

After adding the line for the shell in use, open a new shell or source it once
in the current shell:

```sh
source <(ai init bash)
source <(ai init zsh)
```

Use only the line matching the current shell.

`ai init` prints shell code; `source` installs that code in the current shell so
it can read and replace Bash's Readline buffer or Zsh's ZLE buffer. Re-source
the integration after installing a new `aishell` version to load updated
bindings into an already-running shell.

### Interactive workflow

Tab behaves according to the current command-line state:

| Current state | What Tab does |
| --- | --- |
| The command line is completely empty | Opens the `🤖 AI Prompt ›` prompt. |
| The AI prompt is active | Submits the natural-language request. |
| Any other text is present | Runs normal shell command/path completion. |

The complete generation flow is:

1. Press Tab on an empty command line.
2. Type the desired operation in natural language.
3. Press Tab again, or press Enter. Both keys submit an active AI prompt.
4. The AI prompt is replaced in place by an animated dot spinner such as
   `⠋ ✨ Crafting command…` while `aishell` asks the configured model for either
   a command, a clarifying question, or an answer.
5. A generated command replaces the AI request in the same editable line. It
   remains unexecuted so it can be inspected or changed.
6. Press Enter only after reviewing the generated command to execute it through
   the shell normally.

The visible line changes in place. The arrows below represent successive
contents of one terminal line, not three shell prompts:

```text
Bash: $  ->  $ # 🤖 AI Prompt › list files  ->  $ ls -la
Zsh:  $  ->  🤖 AI Prompt › list files      ->  $ ls -la
```

Bash keeps its normal prompt and puts `# 🤖 AI Prompt › ` in the Readline buffer.
The prefix is a shell comment, so the request is harmless even if another
Readline customization bypasses the integration. Zsh temporarily changes its
ZLE prompt to `🤖 AI Prompt ›` while collecting the request. In both shells, only
the generated command is placed in the final edit buffer.

### Commands, questions, and errors

Shell-widget output is handled by type:

- A command replaces the current edit buffer and moves the cursor to its end.
- A clarifying question or general answer is displayed without inserting text
  that the shell could execute. Press Tab on the empty line again to answer a
  clarification; the bounded command context connects the follow-up.
- A generation error displays diagnostics and never executes the request. Bash
  keeps the request behind its safe comment prefix so it can be edited and
  retried; Zsh returns to an empty command line.

There is no command-or-natural-language classifier on an existing line. For
example, `ec<Tab>` and `git che<Tab>` remain ordinary shell completion. AI mode
starts only from a completely empty buffer, which prevents the integration from
taking over commands or paths the user is already typing.

The integration binds Tab in the Bash and Zsh Emacs and vi-insert keymaps. Bash
also wraps Enter so it can submit an active AI prompt while retaining normal
accept-line behavior everywhere else. A shell setup that assigns custom Tab or
Enter behavior should load the `ai` integration after its completion framework.

Running `ai` without the shell integration prints the generated command to
standard output:

```sh
ai list TCP listeners with process names
```

If no description is passed, `ai` asks for one interactively before generating
the command. This direct CLI mode cannot replace its parent shell's edit buffer;
buffer replacement is performed by the sourced Bash or Zsh integration.

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

## Development builds

Every push to a branch runs the
[`Development build`](.github/workflows/development-build.yml) workflow on
Ubuntu 24.04. It checks formatting, runs the test suite, builds the optimized
Linux x86_64 binary
