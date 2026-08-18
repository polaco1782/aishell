# AIshell

`aishell` turns a natural-language description into a shell command directly
from the interactive command line. The generated command remains editable and
is never executed automatically. It can remember the context so it is possible
to do multiple interactions with the resulting commands.

> [!NOTE]
> Interactive shell integration is available for **Bash, Zsh, and PowerShell**.
> Stock `cmd.exe` can generate CMD commands through the standalone CLI, but its
> built-in editor cannot provide the Tab shortcut or in-place replacement.
>
> Supported AI providers are **OpenRouter**, **OpenAI**, **llama.cpp**, and
> **vLLM**. llama.cpp and vLLM use their OpenAI-compatible Chat Completions
> servers.

## Demo

![Tab opens the aishell prompt, submits a natural-language request, displays its color-coded command risk, and leaves the generated command editable](aishell-demo.gif)

Press Tab on an empty command line, describe the operation, and press Tab again.
The AI request is replaced in place by a command that can be reviewed before
pressing Enter.

## Examples

Requests can be as short or as specific as needed:

```text
show the ten largest files in this project, excluding target
which process is listening on port 8080?
show commits that changed src/provider.rs this month
install ripgrep using my system's package manager
create a tar.gz containing log files modified in the last seven days
what does chmod 2750 mean?
```

Follow-ups reuse the bounded context for the current shell and working tree. For
example:

```text
create a 512 MiB sparse disk image named test.img
format it as ext4
now show me how to mount it read-only under /mnt/test
```

Each generated command is still only inserted into the editable command line.
Review commands carefully—especially package-management, formatting, and
privileged operations—before deciding whether to run them.

## Command risk

Every generated command must include one of three destructive-risk levels.
`aishell` rejects a provider response that omits the risk or uses any value
other than `safe`, `moderate`, or `high`, so an unclassified command is not
inserted into the shell.

| Risk | Message color | Meaning |
| --- | --- | --- |
| `safe` | Light green | Read-only commands with no meaningful side effects, such as inspecting files, processes, or repository state. |
| `moderate` | Light yellow | Bounded, normally recoverable changes to files, packages, processes, or system state. |
| `high` | Light red | Commands that can delete or overwrite data, make broad or difficult-to-reverse changes, expose secrets, execute untrusted remote code, or make a system unusable. |

Classification considers the complete generated command and its context, not
only the executable name. When the correct level is uncertain, the model is
instructed to choose the higher risk. A moderate-risk message, shown in light
yellow, looks like:

```text
⚠  · Command may modify your system or data. Review it carefully.
```

The message appears immediately before the editable command, with no countdown
or automatic execution. With a direct `ai` invocation it is written to stderr,
while the command remains the only stdout output. With the interactive shell
integration it is displayed above the generated command line.

The classification is review guidance produced by the configured model, not a
security boundary or a guarantee that a command is harmless. Always inspect the
complete command before pressing Enter. Setting `safety.risk_warning = false`
hides the visible message only; the provider must still classify every command,
and `aishell` still validates the classification.

## Build and configure

Building requires Rust 1.95 or newer.

The provider, model, endpoint, and optional credentials are configured by
`ai setup`; provider credentials are read from the private configuration file,
not environment variables.

On Linux:

```sh
cargo build --release
cargo install --path .
ai setup
```

On Windows, use PowerShell:

```powershell
cargo build --release
cargo install --path .
ai setup
```

`ai setup` interactively selects OpenRouter, OpenAI, llama.cpp, or vLLM, then
asks for that provider's model and API base URL. API keys are required for
OpenRouter and OpenAI and optional for authenticated llama.cpp or vLLM servers;
key input is never echoed. On Linux, the configuration is written to
`~/.config/aishell/config.toml`, or under an absolute `XDG_CONFIG_HOME`. Its
directory and file are created with modes `0700` and `0600`. On Windows it is
written to `%LOCALAPPDATA%\aishell\config.toml`, under the current user's
profile and inherited access controls. The data directory and private files
reject symlink and Windows reparse-point replacements.

The configuration schema is intentionally current-only. After updating from a
version without the `[provider]` or `[safety]` section, run `ai setup` to replace
the old configuration. An OpenAI example is:

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

[tools]
file_io = false

[safety]
risk_warning = true
```

The `[safety]` setting controls whether the color-coded message described in
[Command risk](#command-risk) is displayed. It defaults to `true`.
Setting `tools.file_io = true` lets the model read and atomically create or
replace UTF-8 text files below the current working directory. Reads and writes
are bounded, and paths cannot escape through absolute paths, parent traversal,
or symbolic links.

Useful configuration commands:

```sh
ai config path
ai config show     # API key is redacted
ai config check    # checks authentication, endpoint, and configured model
ai context path    # prints the central history database path
ai context show    # shows the current bounded context
ai context clear   # forgets the current context
ai logs            # shows model file reads, writes, and modifications
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

### Local llama.cpp setup

If `llama-server` is not already installed, build it from the official
[`llama.cpp`](https://github.com/ggml-org/llama.cpp) repository:

```sh
git clone https://github.com/ggml-org/llama.cpp.git
cmake -S llama.cpp -B llama.cpp/build -DCMAKE_BUILD_TYPE=Release
cmake --build llama.cpp/build --config Release --target llama-server -j
```

Start the OpenAI-compatible server with a GGUF chat model. This small model is a
convenient first test; replace it with another llama.cpp-compatible model that
fits the available RAM or VRAM:

```sh
./llama.cpp/build/bin/llama-server \
  -hf ggml-org/gemma-3-1b-it-GGUF \
  --alias local-command-model \
  --host 127.0.0.1 \
  --port 8080
```

The first launch downloads the model from Hugging Face. `--alias` gives the
served model a stable identifier for `aishell`; leave this server running and,
in another terminal, configure the client:

```sh
ai setup
# Select provider 3 (llama.cpp).
# Leave the optional API key empty.
# Enter local-command-model as the served model.
# Accept http://127.0.0.1:8080/v1 as the API base URL.

ai config check
ai show the five largest files in this directory
```

The equivalent provider section in `~/.config/aishell/config.toml` is:

```toml
[provider]
type = "llamacpp"
model = "local-command-model"
base_url = "http://127.0.0.1:8080/v1"
```

No API key is needed for this loopback-only server. If authentication is added
to the server, add its token as `api_key` in the private configuration file.
If editing the file manually, preserve its `0600` permissions. The
[llama.cpp server](https://github.com/ggml-org/llama.cpp/blob/master/tools/server/README.md)
exposes the `/v1/models` and `/v1/chat/completions` endpoints that `aishell`
uses.

## Shell integration

The edit-buffer integration currently supports these shells:

| Shell | Integration | Configuration |
| --- | --- | --- |
| Bash | Readline | Add `source <(ai init bash)` to `.bashrc`. |
| Zsh | ZLE | Add `source <(ai init zsh)` to `.zshrc`. |
| PowerShell | PSReadLine 2.0+ | Add `ai init powershell \| Out-String \| Invoke-Expression` to `$PROFILE`. |

No Fish, Nushell, stock CMD, or other shell integration is provided yet. Those
shells can still use `ai [description...]` as a normal CLI command, which prints
the generated command instead of inserting it into the current edit buffer.

After adding the line for the shell in use, open a new shell or source it once
in the current shell:

```sh
source <(ai init bash)
source <(ai init zsh)
```

For PowerShell, create its profile if needed, add the integration, and load it:

```powershell
if (-not (Test-Path $PROFILE)) {
    New-Item -ItemType File -Path $PROFILE -Force | Out-Null
}
Add-Content $PROFILE 'ai init powershell | Out-String | Invoke-Expression'
. $PROFILE
```

Use only the setup matching the current shell. The PowerShell adapter targets
PowerShell 7 and Windows PowerShell 5.1 with PSReadLine 2.0 or newer.

`ai init` prints shell code; sourcing or evaluating that installed-binary output
lets it read and replace Bash's Readline, Zsh's ZLE, or PowerShell's PSReadLine
buffer. Reload the integration after installing a new `aishell` version to load
updated bindings into an already-running shell.

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
5. A light green, light yellow, or light red message immediately displays the
   command's safe, moderate, or high destructive-risk classification.
6. A generated command replaces the AI request in the same editable line. It
   remains unexecuted so it can be inspected or changed.
7. Press Enter only after reviewing the generated command to execute it through
   the shell normally.

The visible line changes in place. The arrows below represent successive
contents of one terminal line, not three shell prompts:

```text
Bash: $  ->  $ # 🤖 AI Prompt › list files  ->  $ ls -la
Zsh:  $  ->  🤖 AI Prompt › list files      ->  $ ls -la
PS:   >  ->  > # 🤖 AI Prompt › list files  ->  > Get-ChildItem -Force
```

Bash and PowerShell keep their normal prompt and put `# 🤖 AI Prompt › ` in the
editable buffer. The prefix is a shell comment, so the request is harmless if
another customization bypasses the integration. Zsh temporarily changes its
ZLE prompt to `🤖 AI Prompt ›` while collecting the request. In every supported
shell, only the generated command is placed in the final edit buffer.

### Commands, questions, and errors

Shell-widget output is handled by type:

- A command replaces the current edit buffer and moves the cursor to its end.
- A clarifying question or general answer is displayed without inserting text
  that the shell could execute. Press Tab on the empty line again to answer a
  clarification; the bounded command context connects the follow-up.
- A generation error displays diagnostics and never executes the request. Bash
  and PowerShell keep the request behind their safe comment prefix so it can be
  edited and retried; Zsh returns to an empty command line.

There is no command-or-natural-language classifier on an existing line. For
example, `ec<Tab>` and `git che<Tab>` remain ordinary shell completion. AI mode
starts only from a completely empty buffer, which prevents the integration from
taking over commands or paths the user is already typing.

The integration binds Tab in the Bash and Zsh Emacs and vi-insert keymaps.
PowerShell binds the current PSReadLine mode, including only Insert mode when Vi
editing is active. Bash and PowerShell also wrap Enter so they can submit an
active AI prompt while retaining normal accept-line behavior everywhere else.
A shell setup that assigns custom Tab or Enter behavior should load the `ai`
integration after its completion framework.

Running `ai` without the shell integration prints the generated command to
standard output:

```sh
ai list TCP listeners with process names
```

If no description is passed, `ai` asks for one interactively before generating
the command. This direct CLI mode cannot replace its parent shell's edit buffer;
buffer replacement is performed by an installed Bash, Zsh, or PowerShell
integration. Standalone generation defaults to PowerShell syntax on Windows and
Bash syntax elsewhere.

In stock CMD, request CMD syntax explicitly and review the printed result:

```cmd
ai --shell cmd -- list listening TCP ports with process names
```

CMD has no programmable edit-buffer or Tab-hook interface. Exact CMD parity
would require an optional line editor such as Clink; `aishell` does not install
or inject one.

## Command context

By default, `aishell` sends the previous six requests and generated results with
the next request. This lets follow-ups such as `create a filesystem on it` reuse
the concrete filename selected by an earlier command. The current working
directory is included on every turn.

History is kept in one private SQLite database. Linux uses
`$XDG_STATE_HOME/aishell/history.sqlite3`, or
`~/.local/state/aishell/history.sqlite3` when `XDG_STATE_HOME` is not absolute;
the directory and database use modes `0700` and `0600`. Windows uses
`%LOCALAPPDATA%\aishell\history.sqlite3` and the current user's inherited access
controls. No `.aishell` file or other history is written into a project.
Context is separated by interactive shell session and Git working tree (or the
exact directory when outside Git). Direct invocations without the shell
integration use the working tree or directory as their context.

Saved commands are explicitly treated as unconfirmed: `aishell` knows what it
inserted for review, but cannot assume the user executed it unchanged. Set
`context.enabled = false` to disable history, reduce `context.max_turns` to send
less history, or use `ai context clear` to erase the current context.

Every model file-tool call is appended immediately to the same private database
with the request, attempted operation, path when available, and byte offset for
reads. Successful reads, new-file writes, and existing-file modifications also
record the exact content involved; failed calls record why the read or write was
denied. This audit logging remains active whenever `tools.file_io = true`, even
when conversational context is disabled. Use `ai logs` to read the audit trail
for the current shell session/worktree scope. `ai context clear` also erases that
scope's file I/O log. Logged text is escaped when necessary so terminal control
bytes cannot be replayed by the log viewer.

Each request includes the host OS family and architecture. On Linux, `aishell`
also reads the standard public `/etc/os-release` file, falling back to
`/usr/lib/os-release`, and sends only its distribution ID, version ID, codename,
and distribution-family IDs. This lets the model choose package commands that
match Debian-, Fedora-, and other Linux families without exposing arbitrary
files or environment values.

Use `ai -- <description>` when a description exactly matches one of the
management commands shown above.

No environment variable is required for the API key, model, or endpoint.
