use crate::cli::Shell;

pub fn init_script(shell: Shell) -> &'static str {
    match shell {
        Shell::Bash => BASH_INIT,
        Shell::Zsh => ZSH_INIT,
    }
}

// Bash cannot invoke its normal completion widget from a `bind -x` function.
// The Tab macro therefore runs the AI hook first, followed by a dynamically
// selected completion/no-op binding.
const BASH_INIT: &str = r#"# Keep related requests in one private conversation without writing into the
# working directory. A newly started interactive shell receives a new ID.
if [[ ${AISHELL_SESSION_OWNER_PID-} != "$$" ]]; then
    AISHELL_SESSION_OWNER_PID=$$
    AISHELL_SESSION_ID="bash-$$-$RANDOM-$RANDOM"
    export AISHELL_SESSION_OWNER_PID AISHELL_SESSION_ID
fi

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

__aishell_tab() {
    local generated terminal_state
    local -i generation_status

    # Any content belongs to the shell, with no command-vs-language guessing.
    if [[ -n ${READLINE_LINE-} ]]; then
        __aishell_bind_tab_fallback complete
        return
    fi

    # Readline leaves the terminal in non-canonical mode while a bind-x hook is
    # running. Restore normal input while the CLI owns the AI Command prompt.
    if ! terminal_state=$(command stty -g 2>/dev/null); then
        printf '\n[ai] could not access the terminal\n' >&2
        __aishell_bind_tab_fallback noop
        return
    fi
    printf '\n' >&2
    if ! command stty icanon echo icrnl -inlcr -igncr; then
        command stty "$terminal_state"
        printf '[ai] could not prepare terminal input\n' >&2
        __aishell_bind_tab_fallback noop
        return
    fi
    generated=$(command ai --shell bash)
    generation_status=$?
    command stty "$terminal_state"

    if (( generation_status == 0 )); then
        if [[ -n $generated ]]; then
            READLINE_LINE=$generated
            READLINE_POINT=${#READLINE_LINE}
        fi
    else
        printf '[ai] generation failed\n' >&2
    fi
    __aishell_bind_tab_fallback noop
}

bind -m emacs-standard -x '"\C-x\C-a":__aishell_tab'
bind -m vi-insert -x '"\C-x\C-a":__aishell_tab'
bind -m emacs-standard '"\C-i":"\C-x\C-a\C-x\C-z"'
bind -m vi-insert '"\C-i":"\C-x\C-a\C-x\C-z"'
"#;

const ZSH_INIT: &str = r#"# Re-sourcing the integration keeps this shell's context; a child shell gets a
# distinct conversation even though it inherits the environment.
if [[ ${AISHELL_SESSION_OWNER_PID-} != $$ ]]; then
    typeset -gx AISHELL_SESSION_OWNER_PID=$$
    typeset -gx AISHELL_SESSION_ID="zsh-$$-$RANDOM-$RANDOM"
fi

__aishell_tab() {
    emulate -L zsh
    local generated request
    local original_prompt=$PROMPT
    local original_rprompt=$RPROMPT
    local -i edit_status generation_status

    # During the recursive prompt, Tab must not recursively open another one.
    if (( __aishell_prompt_active )); then
        zle expand-or-complete
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

    zle -I
    print -r -- '[ai] generating command...' >&2
    generated=$(command ai --shell zsh -- "$request")
    generation_status=$?

    if (( generation_status == 0 )); then
        if [[ -n $generated ]]; then
            BUFFER=$generated
            CURSOR=${#BUFFER}
        fi
    else
        print -r -- '[ai] generation failed' >&2
    fi
    zle reset-prompt
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
        assert!(script.contains("if [[ -n ${READLINE_LINE-} ]]"));
        assert!(script.contains("__aishell_bind_tab_fallback complete"));
        assert!(script.contains("generated=$(command ai --shell bash)"));
        assert!(script.contains("READLINE_LINE=$generated"));
        assert!(script.contains("READLINE_POINT=${#READLINE_LINE}"));
        assert!(script.contains("command stty icanon echo icrnl -inlcr -igncr"));
        assert!(script.contains("AISHELL_SESSION_ID=\"bash-$$-$RANDOM-$RANDOM\""));
        assert!(!script.contains("compgen"));
        assert!(!script.contains("accept-line"));
        assert!(!script.contains("__aishell_prompting"));
    }

    #[test]
    fn zsh_uses_ai_only_for_an_empty_buffer() {
        let script = init_script(Shell::Zsh);
        assert!(script.contains("if [[ -n $BUFFER ]]"));
        assert!(script.contains("zle expand-or-complete"));
        assert!(script.contains("PROMPT='AI Command> '"));
        assert!(script.contains("zle recursive-edit"));
        assert!(script.contains("generated=$(command ai --shell zsh -- \"$request\")"));
        assert!(script.contains("BUFFER=$generated"));
        assert!(script.contains("CURSOR=${#BUFFER}"));
        assert!(script.contains("AISHELL_SESSION_ID=\"zsh-$$-$RANDOM-$RANDOM\""));
        assert!(script.contains("bindkey -M $__aishell_keymap '^I' __aishell_tab"));
        assert!(!script.contains("${(k)commands}"));
        assert!(!script.contains("accept-line"));
        assert!(!script.contains("__aishell_prompting"));
    }
}
