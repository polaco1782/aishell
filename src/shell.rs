use anyhow::{Result, bail};

use crate::cli::Shell;
use crate::ui;

pub fn init_script(shell: Shell) -> Result<String> {
    let template = match shell {
        Shell::Bash => BASH_INIT,
        Shell::Zsh => ZSH_INIT,
        Shell::Pwsh => POWERSHELL_INIT,
        Shell::Cmd => bail!("cmd.exe does not support programmable edit-buffer integration"),
    };

    // Keep terminal-facing labels consistent with the standalone CLI.
    Ok(template
        .replace("{{AI_PROMPT}}", ui::AI_PROMPT)
        .replace("{{THINKING}}", ui::THINKING)
        .replace("{{SPINNER_FRAMES}}", ui::SPINNER_FRAMES)
        .replace(
            "{{POWERSHELL_SPINNER_FRAMES}}",
            &ui::SPINNER_FRAMES
                .split_whitespace()
                .map(powershell_string_expression)
                .collect::<Vec<_>>()
                .join(", "),
        )
        .replace(
            "{{POWERSHELL_AI_PROMPT}}",
            &powershell_string_expression(ui::AI_PROMPT),
        )
        .replace(
            "{{POWERSHELL_THINKING}}",
            &powershell_string_expression(ui::THINKING),
        )
        .replace(
            "{{POWERSHELL_ERROR}}",
            &powershell_string_expression(ui::ERROR),
        )
        .replace("{{ERROR}}", ui::ERROR))
}

/// Produces a PowerShell 5.1-compatible expression containing ASCII source
/// only. Windows PowerShell may decode native stdout using an OEM code page,
/// so embedding UTF-8 directly in `ai init powershell` corrupts the script
/// before `Invoke-Expression` sees it.
fn powershell_string_expression(value: &str) -> String {
    let mut parts = Vec::new();
    let mut ascii = String::new();

    let flush_ascii = |ascii: &mut String, parts: &mut Vec<String>| {
        if !ascii.is_empty() {
            parts.push(format!("'{}'", ascii.replace('\'', "''")));
            ascii.clear();
        }
    };

    for character in value.chars() {
        if character.is_ascii() {
            ascii.push(character);
            continue;
        }

        flush_ascii(&mut ascii, &mut parts);
        let code_point = character as u32;
        if code_point <= u16::MAX.into() {
            parts.push(format!("([char]0x{code_point:04X})"));
        } else {
            parts.push(format!("([char]::ConvertFromUtf32(0x{code_point:X}))"));
        }
    }
    flush_ascii(&mut ascii, &mut parts);

    if parts.is_empty() {
        "''".into()
    } else {
        parts.join(" + ")
    }
}

// Bash cannot invoke its normal completion or accept-line widgets from a
// `bind -x` function. The key macros therefore run the AI hook first, followed
// by a dynamically selected built-in/no-op binding.
const BASH_INIT: &str = r#"# Keep related requests in one private conversation without writing into the
# working directory. A newly started interactive shell receives a new ID.
if [[ ${AISHELL_SESSION_OWNER_PID-} != "$$" ]]; then
    AISHELL_SESSION_OWNER_PID=$$
    AISHELL_SESSION_ID="bash-$$-$RANDOM-$RANDOM"
    export AISHELL_SESSION_OWNER_PID AISHELL_SESSION_ID
fi

__aishell_prompt_prefix='# {{AI_PROMPT}}'

__aishell_bind_tab_fallback() {
    local binding=$1
    if [[ $binding == complete ]]; then
        bind -m emacs-standard '"\C-x\C-z": complete'
        bind -m vi-insert '"\C-x\C-z": complete'
    else
        bind -m emacs-standard '"\C-x\C-z": ""'
        bind -m vi-insert '"\C-x\C-z": ""'
    fi
}

__aishell_bind_accept_fallback() {
    local binding=$1
    if [[ $binding == accept ]]; then
        bind -m emacs-standard '"\C-x\C-y": accept-line'
        bind -m vi-insert '"\C-x\C-y": accept-line'
    else
        bind -m emacs-standard '"\C-x\C-y": ""'
        bind -m vi-insert '"\C-x\C-y": ""'
    fi
}

__aishell_generate() {
    local request=${READLINE_LINE#"$__aishell_prompt_prefix"}
    local generated output_file error_file
    local -a spinner_frames=( {{SPINNER_FRAMES}} )
    local -i generation_status spinner_index=0

    # Keep the safe comment prompt in place until there is a real request.
    if [[ -z ${request//[[:space:]]/} ]]; then
        return
    fi

    if ! output_file=$(mktemp "${TMPDIR:-/tmp}/aishell.bash.output.XXXXXXXX"); then
        printf '\r\033[2K{{ERROR}} Could not create a temporary diagnostics file\n' >&2
        return
    fi
    if ! error_file=$(mktemp "${TMPDIR:-/tmp}/aishell.bash.error.XXXXXXXX"); then
        command rm -f -- "$output_file"
        printf '\r\033[2K{{ERROR}} Could not create a temporary diagnostics file\n' >&2
        return
    fi

    # Keep stdout isolated for the editable command. Stream complete stderr
    # lines so the risk message remains visible above the generated command.
    # The foreground subshell owns the background request, keeping Bash job
    # notices hidden and ensuring an interrupt also terminates the generator.
    (
        local generation_pid diagnostic_fd diagnostic_line
        local -i diagnostics_visible=0
        trap 'kill "$generation_pid" 2>/dev/null; wait "$generation_pid" 2>/dev/null; exit 130' INT TERM HUP
        exec {diagnostic_fd}<"$error_file"
        command ai --shell bash -- "$request" >"$output_file" 2>"$error_file" &
        generation_pid=$!
        while kill -0 "$generation_pid" 2>/dev/null; do
            while IFS= read -r diagnostic_line <&"$diagnostic_fd"; do
                if (( diagnostics_visible == 0 )); then
                    printf '\r\033[2K' >&2
                fi
                printf '%s\n' "$diagnostic_line" >&2
                diagnostics_visible=1
            done
            if (( diagnostics_visible == 0 )); then
                printf '\r\033[2K%s {{THINKING}}' "${spinner_frames[spinner_index]}" >&2
                (( spinner_index = (spinner_index + 1) % ${#spinner_frames[@]} ))
            fi
            command sleep 0.08
        done
        wait "$generation_pid"
        generation_status=$?
        while IFS= read -r diagnostic_line <&"$diagnostic_fd"; do
            if (( diagnostics_visible == 0 )); then
                printf '\r\033[2K' >&2
            fi
            printf '%s\n' "$diagnostic_line" >&2
            diagnostics_visible=1
        done
        exec {diagnostic_fd}<&-
        exit "$generation_status"
    )
    generation_status=$?
    generated=$(<"$output_file")
    command rm -f -- "$output_file" "$error_file"
    printf '\r\033[2K' >&2

    if (( generation_status == 0 )); then
        if [[ -n $generated ]]; then
            READLINE_LINE=$generated
            READLINE_POINT=${#READLINE_LINE}
        else
            READLINE_LINE=
            READLINE_POINT=0
        fi
    else
        printf '{{ERROR}} Generation failed; request kept for editing\n' >&2
    fi
}

__aishell_tab() {
    local line=${READLINE_LINE-}

    if [[ $line == "$__aishell_prompt_prefix"* ]]; then
        __aishell_generate
        __aishell_bind_tab_fallback noop
        return
    fi

    # Any other content belongs to the shell, with no command-vs-language
    # guessing. Only a completely empty line enters the AI prompt.
    if [[ -n $line ]]; then
        __aishell_bind_tab_fallback complete
        return
    fi

    # The prefix is a shell comment, so it remains harmless even if another
    # Readline customization bypasses the accept hook below.
    READLINE_LINE=$__aishell_prompt_prefix
    READLINE_POINT=${#READLINE_LINE}
    __aishell_bind_tab_fallback noop
}

__aishell_accept_or_generate() {
    if [[ ${READLINE_LINE-} == "$__aishell_prompt_prefix"* ]]; then
        __aishell_generate
        # Leave the generated command in Readline for review.
        __aishell_bind_accept_fallback noop
    else
        __aishell_bind_accept_fallback accept
    fi
}

bind -m emacs-standard -x '"\C-x\C-a":__aishell_tab'
bind -m vi-insert -x '"\C-x\C-a":__aishell_tab'
bind -m emacs-standard '"\C-i":"\C-x\C-a\C-x\C-z"'
bind -m vi-insert '"\C-i":"\C-x\C-a\C-x\C-z"'
bind -m emacs-standard -x '"\C-x\C-e":__aishell_accept_or_generate'
bind -m vi-insert -x '"\C-x\C-e":__aishell_accept_or_generate'
bind -m emacs-standard '"\C-m":"\C-x\C-e\C-x\C-y"'
bind -m vi-insert '"\C-m":"\C-x\C-e\C-x\C-y"'
bind -m emacs-standard '"\C-j":"\C-x\C-e\C-x\C-y"'
bind -m vi-insert '"\C-j":"\C-x\C-e\C-x\C-y"'
"#;

const ZSH_INIT: &str = r#"# Re-sourcing the integration keeps this shell's context; a child shell gets a
# distinct conversation even though it inherits the environment.
if [[ ${AISHELL_SESSION_OWNER_PID-} != $$ ]]; then
    typeset -gx AISHELL_SESSION_OWNER_PID=$$
    typeset -gx AISHELL_SESSION_ID="zsh-$$-$RANDOM-$RANDOM"
fi

__aishell_submit_prompt() {
    typeset -g __aishell_prompt_submitted=1
    # Leave the recursive editor without accepting a shell command or adding a
    # new terminal line. The outer widget owns the request and final buffer.
    zle .send-break
}

__aishell_tab() {
    emulate -L zsh
    # Background generation is private widget work, not a user-visible job.
    unsetopt MONITOR NOTIFY
    setopt LOCAL_TRAPS
    local generated request output_file error_file generation_pid diagnostic_fd diagnostic_line
    local original_prompt=$PROMPT
    local original_rprompt=$RPROMPT
    local -a spinner_frames=( {{SPINNER_FRAMES}} )
    local -i generation_status spinner_index=1 diagnostics_visible=0

    # During the recursive prompt, Tab must not recursively open another one.
    if (( __aishell_prompt_active )); then
        __aishell_submit_prompt
        return
    fi

    # Any content belongs to ZLE, with no command-vs-language guessing.
    if [[ -n $BUFFER ]]; then
        zle expand-or-complete
        return
    fi

    # A recursive edit gives the request its own prompt without executing the
    # natural-language buffer when Enter is pressed.
    PROMPT='{{AI_PROMPT}}'
    RPROMPT=
    typeset -g __aishell_prompt_active=1
    typeset -g __aishell_prompt_submitted=0
    # Enter should submit the recursive AI editor without accepting its text as
    # a shell command. Preserve any existing themed accept-line widget.
    zle -A accept-line __aishell_saved_accept_line
    zle -N accept-line __aishell_submit_prompt
    zle reset-prompt
    zle recursive-edit
    zle -A __aishell_saved_accept_line accept-line
    zle -D __aishell_saved_accept_line
    typeset -g __aishell_prompt_active=0
    request=$BUFFER
    BUFFER=
    CURSOR=0

    if (( ! __aishell_prompt_submitted )) || [[ -z ${request//[[:space:]]/} ]]; then
        PROMPT=$original_prompt
        RPROMPT=$original_rprompt
        zle reset-prompt
        return
    fi

    # Put the status in ZLE's real buffer so its cursor sits after the text;
    # POSTDISPLAY would leave the cursor blinking underneath the emoji.
    PROMPT=
    RPROMPT=
    zle reset-prompt
    if ! output_file=$(mktemp "${TMPDIR:-/tmp}/aishell.zsh.output.XXXXXXXX"); then
        BUFFER=
        CURSOR=0
        PROMPT=$original_prompt
        RPROMPT=$original_rprompt
        zle reset-prompt
        zle -M '{{ERROR}} Could not create a temporary diagnostics file'
        return
    fi
    if ! error_file=$(mktemp "${TMPDIR:-/tmp}/aishell.zsh.error.XXXXXXXX"); then
        command rm -f -- "$output_file"
        BUFFER=
        CURSOR=0
        PROMPT=$original_prompt
        RPROMPT=$original_rprompt
        zle reset-prompt
        zle -M '{{ERROR}} Could not create a temporary diagnostics file'
        return
    fi

    exec {diagnostic_fd}<"$error_file"
    command ai --shell zsh -- "$request" >"$output_file" 2>"$error_file" &
    generation_pid=$!
    trap 'kill "$generation_pid" 2>/dev/null' INT TERM HUP
    while kill -0 "$generation_pid" 2>/dev/null; do
        while IFS= read -r diagnostic_line <&"$diagnostic_fd"; do
            if (( diagnostics_visible == 0 )); then
                BUFFER=
                CURSOR=0
                zle -R
            fi
            printf '%s\n' "$diagnostic_line" >&2
            diagnostics_visible=1
        done
        if (( diagnostics_visible == 0 )); then
            BUFFER="${spinner_frames[spinner_index]} {{THINKING}}"
            CURSOR=${#BUFFER}
            zle -R
            (( spinner_index = spinner_index % ${#spinner_frames} + 1 ))
        fi
        command sleep 0.08
    done
    wait "$generation_pid"
    generation_status=$?
    while IFS= read -r diagnostic_line <&"$diagnostic_fd"; do
        if (( diagnostics_visible == 0 )); then
            BUFFER=
            CURSOR=0
            zle -R
        fi
        printf '%s\n' "$diagnostic_line" >&2
        diagnostics_visible=1
    done
    exec {diagnostic_fd}<&-
    generated=$(<"$output_file")
    command rm -f -- "$output_file" "$error_file"

    BUFFER=
    CURSOR=0
    if (( generation_status == 0 )); then
        if [[ -n $generated ]]; then
            BUFFER=$generated
            CURSOR=${#BUFFER}
        fi
    fi
    PROMPT=$original_prompt
    RPROMPT=$original_rprompt
    zle reset-prompt
    if (( generation_status != 0 )); then
        zle -M '{{ERROR}} Generation failed'
    fi
}

typeset -gi __aishell_prompt_active=0
typeset -gi __aishell_prompt_submitted=0
zle -N __aishell_tab
zle -N __aishell_submit_prompt
for __aishell_keymap in emacs viins; do
    bindkey -M $__aishell_keymap '^I' __aishell_tab
done
unset __aishell_keymap
"#;

const POWERSHELL_INIT: &str = r#"# Keep related requests in one private conversation without writing into the
# working directory. A newly started PowerShell process receives a new ID.
if ($env:AISHELL_SESSION_OWNER_PID -ne [string]$PID) {
    $env:AISHELL_SESSION_OWNER_PID = [string]$PID
    $env:AISHELL_SESSION_ID = "powershell-$PID-$([Guid]::NewGuid().ToString('N'))"
}

Import-Module PSReadLine -MinimumVersion 2.0 -ErrorAction Stop

$global:__AishellPromptPrefix = '# ' + ({{POWERSHELL_AI_PROMPT}})
$global:__AishellThinking = {{POWERSHELL_THINKING}}
$global:__AishellError = {{POWERSHELL_ERROR}}
$global:__AishellSpinnerFrames = @({{POWERSHELL_SPINNER_FRAMES}})
$global:__AishellUtf8Encoding = New-Object System.Text.UTF8Encoding -ArgumentList $false

function global:Invoke-AishellUtf8Console {
    param([scriptblock]$Action)

    if ($PSVersionTable.PSEdition -ne 'Desktop') {
        & $Action
        return
    }

    $previousEncoding = [Console]::OutputEncoding
    try {
        [Console]::OutputEncoding = $global:__AishellUtf8Encoding
        & $Action
    }
    finally {
        [Console]::OutputEncoding = $previousEncoding
    }
}

function global:Set-AishellBuffer {
    param([string]$Text = '')

    $line = $null
    [int]$cursor = 0
    [Microsoft.PowerShell.PSConsoleReadLine]::GetBufferState([ref]$line, [ref]$cursor)
    Invoke-AishellUtf8Console {
        [Microsoft.PowerShell.PSConsoleReadLine]::Replace(0, $line.Length, $Text)
    }
}

function global:Write-AishellErrorLine {
    param([AllowEmptyString()][string]$Message = '')

    Invoke-AishellUtf8Console {
        [Console]::Error.WriteLine($Message)
    }
}

function global:Write-AishellDiagnostics {
    param([string]$Message)

    if ([string]::IsNullOrWhiteSpace($Message)) {
        return
    }
    Write-AishellErrorLine
    Write-AishellErrorLine ($Message.TrimEnd([char[]]@("`r", "`n")))
    [Microsoft.PowerShell.PSConsoleReadLine]::InvokePrompt()
}

function global:Invoke-AishellGeneration {
    param([string]$Line)

    $request = $Line.Substring($global:__AishellPromptPrefix.Length)
    if ([string]::IsNullOrWhiteSpace($request)) {
        return
    }

    $process = $null
    $started = $false
    $generated = ''
    $diagnostics = ''
    $diagnosticsVisible = $false
    [int]$generationStatus = 1

    try {
        $aiCommand = Get-Command ai -CommandType Application -ErrorAction Stop | Select-Object -First 1
        $location = Get-Location
        if ($location.Provider.Name -ne 'FileSystem') {
            throw 'aishell requires a filesystem working directory'
        }

        $utf8 = New-Object System.Text.UTF8Encoding -ArgumentList $false
        $startInfo = New-Object System.Diagnostics.ProcessStartInfo
        $startInfo.FileName = $aiCommand.Path
        $startInfo.Arguments = '--shell powershell --stdin'
        $startInfo.WorkingDirectory = $location.ProviderPath
        $startInfo.UseShellExecute = $false
        $startInfo.CreateNoWindow = $true
        $startInfo.RedirectStandardInput = $true
        $startInfo.RedirectStandardOutput = $true
        $startInfo.RedirectStandardError = $true
        $startInfo.StandardOutputEncoding = $utf8
        $startInfo.StandardErrorEncoding = $utf8

        $process = New-Object System.Diagnostics.Process
        $process.StartInfo = $startInfo
        if (-not $process.Start()) {
            throw 'could not start ai.exe'
        }
        $started = $true
        $stdoutTask = $process.StandardOutput.ReadToEndAsync()
        $stderrLineTask = $process.StandardError.ReadLineAsync()
        $requestBytes = $utf8.GetBytes($request)
        $process.StandardInput.BaseStream.Write($requestBytes, 0, $requestBytes.Length)
        $process.StandardInput.BaseStream.Flush()
        $process.StandardInput.Close()

        [int]$spinnerIndex = 0
        do {
            while ($stderrLineTask.IsCompleted) {
                $diagnosticLine = $stderrLineTask.Result
                if ($null -eq $diagnosticLine) {
                    break
                }
                if (-not $diagnosticsVisible) {
                    Set-AishellBuffer
                    Write-AishellErrorLine
                }
                Write-AishellErrorLine $diagnosticLine
                $diagnosticsVisible = $true
                $stderrLineTask = $process.StandardError.ReadLineAsync()
            }
            if (-not $diagnosticsVisible) {
                Set-AishellBuffer ("{0} {1}" -f $global:__AishellSpinnerFrames[$spinnerIndex], $global:__AishellThinking)
                $spinnerIndex = ($spinnerIndex + 1) % $global:__AishellSpinnerFrames.Count
            }
            $finished = $process.WaitForExit(80)
        } while (-not $finished)

        $process.WaitForExit()
        while ($null -ne $stderrLineTask) {
            $diagnosticLine = $stderrLineTask.Result
            if ($null -eq $diagnosticLine) {
                break
            }
            if (-not $diagnosticsVisible) {
                Set-AishellBuffer
                Write-AishellErrorLine
            }
            Write-AishellErrorLine $diagnosticLine
            $diagnosticsVisible = $true
            $stderrLineTask = $process.StandardError.ReadLineAsync()
        }
        $generated = $stdoutTask.Result.TrimEnd([char[]]@("`r", "`n"))
        $generationStatus = $process.ExitCode
    }
    catch {
        $diagnostics = $_.Exception.Message
    }
    finally {
        if ($started -and -not $process.HasExited) {
            try {
                $process.Kill()
                $process.WaitForExit()
            }
            catch {
            }
        }
        if ($null -ne $process) {
            $process.Dispose()
        }
    }

    [Microsoft.PowerShell.PSConsoleReadLine]::RevertLine()
    if ($generationStatus -eq 0) {
        if (-not [string]::IsNullOrEmpty($generated)) {
            [Microsoft.PowerShell.PSConsoleReadLine]::Insert($generated)
        }
    }
    else {
        [Microsoft.PowerShell.PSConsoleReadLine]::Insert($Line)
        if ($diagnosticsVisible) {
            $diagnostics = $global:__AishellError + ' Generation failed; request kept for editing'
        }
        elseif (-not [string]::IsNullOrWhiteSpace($diagnostics)) {
            $diagnostics += "`n"
            $diagnostics += $global:__AishellError + ' Generation failed; request kept for editing'
        }
        else {
            $diagnostics = $global:__AishellError + ' Generation failed; request kept for editing'
        }
    }
    Write-AishellDiagnostics $diagnostics
}

$global:__AishellTabHandler = {
    param($key, $arg)

    $line = $null
    [int]$cursor = 0
    [Microsoft.PowerShell.PSConsoleReadLine]::GetBufferState([ref]$line, [ref]$cursor)
    if ($line.StartsWith($global:__AishellPromptPrefix, [StringComparison]::Ordinal)) {
        Invoke-AishellGeneration $line
        return
    }
    if ($line.Length -ne 0) {
        if ((Get-PSReadLineOption).EditMode -eq 'Vi') {
            [Microsoft.PowerShell.PSConsoleReadLine]::ViTabCompleteNext()
        }
        else {
            [Microsoft.PowerShell.PSConsoleReadLine]::TabCompleteNext()
        }
        return
    }
    Invoke-AishellUtf8Console {
        [Microsoft.PowerShell.PSConsoleReadLine]::Insert($global:__AishellPromptPrefix)
        # Windows PowerShell 5.1 / PSReadLine 2.0 can render programmatically
        # inserted Unicode as fallback characters until the next keypress
        # forces a complete redraw. InvokePrompt performs it immediately.
        if ($PSVersionTable.PSEdition -eq 'Desktop') {
            [Microsoft.PowerShell.PSConsoleReadLine]::InvokePrompt()
        }
    }
}

$global:__AishellEnterHandler = {
    param($key, $arg)

    $line = $null
    [int]$cursor = 0
    [Microsoft.PowerShell.PSConsoleReadLine]::GetBufferState([ref]$line, [ref]$cursor)
    if ($line.StartsWith($global:__AishellPromptPrefix, [StringComparison]::Ordinal)) {
        Invoke-AishellGeneration $line
    }
    else {
        [Microsoft.PowerShell.PSConsoleReadLine]::AcceptLine()
    }
}

$editMode = (Get-PSReadLineOption).EditMode
if ($editMode -eq 'Vi') {
    Set-PSReadLineKeyHandler -Chord Tab -ViMode Insert -BriefDescription Aishell `
        -Description 'Open or submit the aishell prompt, otherwise complete input' `
        -ScriptBlock $global:__AishellTabHandler
    Set-PSReadLineKeyHandler -Chord Enter -ViMode Insert -BriefDescription AishellAccept `
        -Description 'Submit the aishell prompt, otherwise accept input' `
        -ScriptBlock $global:__AishellEnterHandler
}
else {
    Set-PSReadLineKeyHandler -Chord Tab -BriefDescription Aishell `
        -Description 'Open or submit the aishell prompt, otherwise complete input' `
        -ScriptBlock $global:__AishellTabHandler
    Set-PSReadLineKeyHandler -Chord Enter -BriefDescription AishellAccept `
        -Description 'Submit the aishell prompt, otherwise accept input' `
        -ScriptBlock $global:__AishellEnterHandler
}
Remove-Variable editMode
"#;

#[cfg(test)]
mod tests {
    use super::{Shell, init_script};

    #[test]
    fn bash_uses_ai_only_for_an_empty_buffer() {
        let script = init_script(Shell::Bash).unwrap();
        assert!(script.contains("__aishell_prompt_prefix='# 🤖 AI Prompt › '"));
        assert!(script.contains("if [[ -n $line ]]"));
        assert!(script.contains("__aishell_bind_tab_fallback complete"));
        assert!(script.contains("command ai --shell bash -- \"$request\" >\"$output_file\""));
        assert!(script.contains("READLINE_LINE=$generated"));
        assert!(script.contains("READLINE_POINT=${#READLINE_LINE}"));
        assert!(script.contains("__aishell_accept_or_generate"));
        assert!(script.contains("✨ Crafting command…"));
        assert!(script.contains("spinner_frames=( ⠋ ⠙ ⠹ ⠸ ⠼ ⠴ ⠦ ⠧ ⠇ ⠏ )"));
        assert!(script.contains("command sleep 0.08"));
        assert!(script.contains("exec {diagnostic_fd}<\"$error_file\""));
        assert!(script.contains("read -r diagnostic_line <&\"$diagnostic_fd\""));
        assert!(script.contains("aishell.bash.output.XXXXXXXX"));
        assert!(script.contains("aishell.bash.error.XXXXXXXX"));
        assert!(!script.contains("{{"));
        assert!(script.contains("AISHELL_SESSION_ID=\"bash-$$-$RANDOM-$RANDOM\""));
        assert!(!script.contains("compgen"));
        assert!(!script.contains("command stty"));
    }

    #[test]
    fn zsh_uses_ai_only_for_an_empty_buffer() {
        let script = init_script(Shell::Zsh).unwrap();
        assert!(script.contains("if [[ -n $BUFFER ]]"));
        assert!(script.contains("zle expand-or-complete"));
        assert!(script.contains("PROMPT='🤖 AI Prompt › '"));
        assert!(script.contains("zle recursive-edit"));
        assert!(script.contains("zle .send-break"));
        assert!(script.contains("zle -A accept-line __aishell_saved_accept_line"));
        assert!(script.contains("zle -A __aishell_saved_accept_line accept-line"));
        assert!(script.contains("command ai --shell zsh -- \"$request\" >\"$output_file\""));
        assert!(script.contains("BUFFER=$generated"));
        assert!(script.contains("CURSOR=${#BUFFER}"));
        assert!(
            script.contains("BUFFER=\"${spinner_frames[spinner_index]} ✨ Crafting command…\"")
        );
        assert!(script.contains("command sleep 0.08"));
        assert!(script.contains("exec {diagnostic_fd}<\"$error_file\""));
        assert!(script.contains("read -r diagnostic_line <&\"$diagnostic_fd\""));
        assert!(script.contains("unsetopt MONITOR NOTIFY"));
        assert!(script.contains("aishell.zsh.output.XXXXXXXX"));
        assert!(script.contains("aishell.zsh.error.XXXXXXXX"));
        assert!(script.contains("AISHELL_SESSION_ID=\"zsh-$$-$RANDOM-$RANDOM\""));
        assert!(script.contains("bindkey -M $__aishell_keymap '^I' __aishell_tab"));
        assert!(!script.contains("${(k)commands}"));
        assert!(!script.contains("zle -R '✨ Crafting command…'"));
        assert!(!script.contains("\n    POSTDISPLAY="));
        assert!(!script.contains("{{"));
    }

    #[test]
    fn powershell_uses_psreadline_without_executing_generated_commands() {
        let script = init_script(Shell::Pwsh).unwrap();
        assert!(script.contains("Import-Module PSReadLine -MinimumVersion 2.0"));
        assert!(script.is_ascii());
        assert!(script.contains("[char]::ConvertFromUtf32(0x1F916)"));
        assert!(script.contains("[char]0x203A"));
        assert!(script.contains("$global:__AishellPromptPrefix = '# ' + ("));
        assert!(script.contains("$global:__AishellThinking = ([char]0x2728)"));
        assert!(script.contains("$global:__AishellError = ([char]0x2717)"));
        assert!(script.contains("GetBufferState([ref]$line, [ref]$cursor)"));
        assert!(script.contains("::Replace(0, $line.Length, $Text)"));
        assert!(script.contains("::TabCompleteNext()"));
        assert!(script.contains("::ViTabCompleteNext()"));
        assert!(script.contains("::AcceptLine()"));
        assert!(script.contains("--shell powershell --stdin"));
        assert!(script.contains("RedirectStandardInput = $true"));
        assert!(script.contains("$process.StandardInput.BaseStream.Write($requestBytes"));
        assert!(script.contains("$process.WaitForExit(80)"));
        assert!(script.contains("$process.StandardError.ReadLineAsync()"));
        assert!(script.contains("Write-AishellErrorLine $diagnosticLine"));
        assert!(script.contains("::Insert($generated)"));
        assert!(script.contains("[Console]::OutputEncoding = $global:__AishellUtf8Encoding"));
        assert!(script.contains("[Console]::OutputEncoding = $previousEncoding"));
        assert!(script.contains("function global:Write-AishellErrorLine"));
        assert!(script.contains("$PSVersionTable.PSEdition -eq 'Desktop'"));
        assert!(script.contains("[Microsoft.PowerShell.PSConsoleReadLine]::InvokePrompt()"));
        assert!(!script.contains("Invoke-Expression"));
        assert!(!script.contains(
            "::AcceptLine()\n        [Microsoft.PowerShell.PSConsoleReadLine]::Insert($generated)"
        ));
        assert!(script.contains("([char]0x280B), ([char]0x2819), ([char]0x2839)"));
        assert!(script.contains("AISHELL_SESSION_ID = \"powershell-$PID-"));
        assert!(!script.contains("{{"));
    }

    #[test]
    fn cmd_has_no_native_init_script() {
        assert!(init_script(Shell::Cmd).is_err());
    }
}
