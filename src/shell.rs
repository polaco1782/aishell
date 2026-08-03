use crate::cli::Shell;

pub fn init_script(shell: Shell) -> &'static str {
    match shell {
        Shell::Bash => BASH_INIT,
        Shell::Zsh => ZSH_INIT,
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

__aishell_prompt_prefix='# AI Command> '

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
    local generated error_file error_text
    local -i generation_status

    # Keep the safe comment prompt in place until there is a real request.
    if [[ -z ${request//[[:space:]]/} ]]; then
        return
    fi

    if ! error_file=$(mktemp "${TMPDIR:-/tmp}/aishell.bash.XXXXXXXX"); then
        printf '\r\033[2K[ai] could not create a temporary diagnostics file\n' >&2
        return
    fi

    # Do not let child diagnostics invalidate Readline's idea of the current
    # prompt geometry. A successful command redraws over this status in place.
    printf '\r\033[2K[ai] generating command...' >&2
    generated=$(command ai --shell bash -- "$request" 2>"$error_file")
    generation_status=$?
    error_text=$(<"$error_file")
    command rm -f -- "$error_file"
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
        printf '[ai] generation failed; request kept\n' >&2
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

__aishell_tab() {
    emulate -L zsh
    local generated request error_file error_text status_message
    local original_prompt=$PROMPT
    local original_rprompt=$RPROMPT
    local -i edit_status generation_status

    # During the recursive prompt, Tab must not recursively open another one.
    if (( __aishell_prompt_active )); then
        zle accept-line
        return
    fi

    # Any content belongs to ZLE, with no command-vs-language guessing.
    if [[ -n $BUFFER ]]; then
        zle expand-or-complete
        return
    fi

    # A recursive edit gives the request its own prompt without executing the
    # natural-language buffer when Enter is pressed.
    PROMPT='AI Command> '
    RPROMPT=
    typeset -g __aishell_prompt_active=1
    zle reset-prompt
    zle recursive-edit
    edit_status=$?
    typeset -g __aishell_prompt_active=0
    request=$BUFFER
    BUFFER=
    CURSOR=0
    PROMPT=$original_prompt
    RPROMPT=$original_rprompt

    if (( edit_status != 0 )) || [[ -z ${request//[[:space:]]/} ]]; then
        zle reset-prompt
        return
    fi

    zle -R '[ai] generating command...'
    if ! error_file=$(mktemp "${TMPDIR:-/tmp}/aishell.zsh.XXXXXXXX"); then
        zle reset-prompt
        zle -M '[ai] could not create a temporary diagnostics file'
        return
    fi
    generated=$(command ai --shell zsh -- "$request" 2>"$error_file")
    generation_status=$?
    error_text=$(<"$error_file")
    command rm -f -- "$error_file"

    if (( generation_status == 0 )); then
        if [[ -n $generated ]]; then
            BUFFER=$generated
            CURSOR=${#BUFFER}
        fi
        status_message=$error_text
    else
        status_message=${error_text:+$error_text$'\n'}'[ai] generation failed'
    fi
    zle reset-prompt
    if [[ -n $status_message ]]; then
        zle -M "$status_message"
    fi
}

typeset -gi __aishell_prompt_active=0
zle -N __aishell_tab
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
        assert!(script.contains("__aishell_prompt_prefix='# AI Command> '"));
        assert!(script.contains("if [[ -n $line ]]"));
        assert!(script.contains("__aishell_bind_tab_fallback complete"));
        assert!(script.contains("generated=$(command ai --shell bash -- \"$request\""));
        assert!(script.contains("READLINE_LINE=$generated"));
        assert!(script.contains("READLINE_POINT=${#READLINE_LINE}"));
        assert!(script.contains("__aishell_accept_or_generate"));
        assert!(script.contains("[ai] generating command..."));
        assert!(script.contains("AISHELL_SESSION_ID=\"bash-$$-$RANDOM-$RANDOM\""));
        assert!(!script.contains("compgen"));
        assert!(!script.contains("command stty"));
    }

    #[test]
    fn zsh_uses_ai_only_for_an_empty_buffer() {
        let script = init_script(Shell::Zsh);
        assert!(script.contains("if [[ -n $BUFFER ]]"));
        assert!(script.contains("zle expand-or-complete"));
        assert!(script.contains("PROMPT='AI Command> '"));
        assert!(script.contains("zle recursive-edit"));
        assert!(script.contains("if (( __aishell_prompt_active )); then\n        zle accept-line"));
        assert!(script.contains("generated=$(command ai --shell zsh -- \"$request\""));
        assert!(script.contains("BUFFER=$generated"));
        assert!(script.contains("CURSOR=${#BUFFER}"));
        assert!(script.contains("zle -R '[ai] generating command...'"));
        assert!(script.contains("AISHELL_SESSION_ID=\"zsh-$$-$RANDOM-$RANDOM\""));
        assert!(script.contains("bindkey -M $__aishell_keymap '^I' __aishell_tab"));
        assert!(!script.contains("${(k)commands}"));
        assert!(!script.contains("print -r -- '[ai] generating command...'"));
    }
}
