# aishell

`aishell` turns a natural-language description into a shell command directly
from the interactive command line:

```console
$ run qemu with disk image xyz.vdi<Tab>
$ qemu-system-x86_64 -drive file=xyz.vdi,format=vdi
```

The generated command replaces the current Bash or Zsh edit buffer. It is never
executed automatically, so it can be reviewed and edited before pressing Enter.

## Build and configure

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
```

Useful configuration commands:

```sh
ai config path
ai config show     # API key is redacted
ai config check    # validates the key with OpenRouter
```

## Shell integration

Add the matching line to `.bashrc` or `.zshrc`:

```sh
source <(ai init bash)
source <(ai init zsh)
```

Tab gives normal shell completion priority. If the first token matches a
command, alias, function, shell keyword, or path candidate, the shell handles
the key normally. Otherwise the entire line is treated as natural language and
replaced with the generated command. Generation only activates with the cursor
at the end of a nonempty line.

Some natural-language requests begin with real command names, such as `make a
directory`. Use `ai make a directory<Tab>` to force generation in those cases.
With a bare `ai`, pressing Enter or Tab asks what command you want and leaves an
`ai ` prefix in the real edit buffer. Type the description after it and press
Enter. When generation finishes, the result is placed in the edit buffer as
though you had typed it, but is not executed. If the request is ambiguous, `ai`
asks a clarifying question and keeps the original input so more detail can be
added. If generation fails, the original input remains untouched and the error
is displayed.

The integration binds Tab and Enter in the Bash and Zsh Emacs and vi-insert
keymaps. Enter otherwise retains its normal behavior. A
shell setup that assigns custom behavior directly to either key should load the
`ai` integration after its completion framework.

Running `ai` without the shell integration prints the generated command to
standard output:

```sh
ai list TCP listeners with process names
```

If no description is passed, `ai` asks for one interactively before generating
the command.

Use `ai -- <description>` when a description exactly matches one of the
management commands shown above.

No environment variable is required for the API key, model, or endpoint.
