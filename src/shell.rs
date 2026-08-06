use crate::cli::Shell;
use crate::ui;

pub fn init_script(shell: Shell) -> String {
    let template = match shell {
        Shell::Bash => BASH_INIT,
        Shell::Zsh => ZSH_INIT,
    };

    // Keep terminal-facing labels consistent with the standalone CLI.
    template
        .replace("{{AI_PROMPT}}", ui::AI_PROMPT)
        .replace("{{THINKING}}", ui::THINKING)
        .replace("{{SPINNER_FRAMES}}", ui::SPINNER_FRAMES)
        .replace("{{ERROR}}", ui::ERROR)
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
    local generated output_file error_file error_text
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

    # Do not let child diagnostics invalidate Readline's idea of the current
    # prompt geometry. Animate in place while stdout and stderr stay isolated.
    # The foreground subshell owns the background request, keeping Bash job
    # notices hidden and ensuring an interrupt also terminates the generator.
    (
        local generation_pid
        trap 'kill "$generation_pid" 2>/dev/null; wait "$generation_pid" 2>/dev/null; exit 130' INT TERM HUP
        command ai --shell bash -- "$request" >"$output_file" 2>"$error_file" &
        generation_pid=$!
        while kill -0 "$generation_pid" 2>/dev/null; do
            printf '\r\033[2K%s {{THINKING}}' "${spinner_frames[spinner_index]}" >&2
            (( spinner_index = (spinner_index + 1) % ${#spinner_frames[@]} ))
            command sleep 0.08
        done
        wait "$generation_pid"
    )
    generation_status=$?
    generated=$(<"$output_file")
    error_text=$(<"$error_file")
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
        if [[ -n $error_text ]]; then
            printf '%s\n' "$error_text" >&2
        fi
        printf '{{ERROR}} Generation failed; request kept for editing\n' >&2
    fi

    if (( generation_status == 0 )) && [[ -n $error_text ]]; then
        printf '%s\n' "$error_text" >&2
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
    local generated request output_file error_file error_text status_message generation_pid
    local original_prompt=$PROMPT
    local original_rprompt=$RPROMPT
    local -a spinner_frames=( {{SPINNER_FRAMES}} )
    local -i generation_status spinner_index=1

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

    command ai --shell zsh -- "$request" >"$output_file" 2>"$error_file" &
    generation_pid=$!
    trap 'kill "$generation_pid" 2>/dev/null' INT TERM HUP
    while kill -0 "$generation_pid" 2>/dev/null; do
        BUFFER="${spinner_frames[spinner_index]} {{THINKING}}"
        CURSOR=${#BUFFER}
        zle -R
        (( spinner_index = spinner_index % ${#spinner_frames} + 1 ))
        command sleep 0.08
    done
    wait "$generation_pid"
    generation_status=$?
    generated=$(<"$output_file")
    error_text=$(<"$error_file")
    command rm -f -- "$output_file" "$error_file"

    BUFFER=
    CURSOR=0
    if (( generation_status == 0 )); then
        if [[ -n $generated ]]; then
            BUFFER=$generated
            CURSOR=${#BUFFER}
        fi
        status_message=$error_text
    else
        status_message=${error_text:+$error_text$'\n'}'{{ERROR}} Generation failed'
    fi
    PROMPT=$original_prompt
    RPROMPT=$original_rprompt
    zle reset-prompt
    if [[ -n $status_message ]]; then
        zle -M "$status_message"
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

#[cfg(test)]
mod tests {
    use super::{Shell, init_script};

    #[test]
    fn bash_uses_ai_only_for_an_empty_buffer() {
        let script = init_script(Shell::Bash);
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
        assert!(script.contains("aishell.bash.output.XXXXXXXX"));
        assert!(script.contains("aishell.bash.error.XXXXXXXX"));
        assert!(!script.contains("{{"));
        assert!(script.contains("AISHELL_SESSION_ID=\"bash-$$-$RANDOM-$RANDOM\""));
        assert!(!script.contains("compgen"));
        assert!(!script.contains("command stty"));
    }

    #[test]
    fn zsh_uses_ai_only_for_an_empty_buffer() {
        let script = init_script(Shell::Zsh);
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
}
