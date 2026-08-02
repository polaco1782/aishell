use crate::cli::Shell;

pub fn init_script(shell: Shell) -> &'static str {
    match shell {
        Shell::Bash => BASH_INIT,
        Shell::Zsh => ZSH_INIT,
    }
}

// Readline cannot replace the whole line from programmable completion. This widget
// uses Readline macros to choose between normal completion and whole-buffer generation.
const BASH_INIT: &str = r#"__aishell_noop() {
    :
}

__aishell_prompting=0

__aishell_has_shell_match() {
    local line=$1
    local first initial has_arguments=0

    line=${line#"${line%%[![:space:]]*}"}
    first=${line%%[[:space:]]*}
    [[ -z $first ]] && return 0
    [[ $line == *[[:space:]]* ]] && has_arguments=1

    initial=${first:0:1}
    if [[ ! $initial =~ ^[[:alnum:]_]$ || $first == *=* ]]; then
        return 0
    fi

    if (( has_arguments )); then
        type -t -- "$first" >/dev/null 2>&1 && return 0
        [[ -e $first ]] && return 0
    else
        [[ -n $(compgen -A command -- "$first" 2>/dev/null) ]] && return 0
        [[ -n $(compgen -f -- "$first" 2>/dev/null) ]] && return 0
    fi
    return 1
}

__aishell_bind_fallback() {
    local binding=$1
    if [[ $binding == complete ]]; then
        bind -m emacs-standard '"\C-x\C-z": complete'
        bind -m vi-insert '"\C-x\C-z": complete'
    else
        bind -m emacs-standard -x '"\C-x\C-z":__aishell_noop'
        bind -m vi-insert -x '"\C-x\C-z":__aishell_noop'
    fi
}

__aishell_bind_accept_fallback() {
    local binding=$1
    if [[ $binding == accept ]]; then
        bind -m emacs-standard '"\C-x\C-y": accept-line'
        bind -m vi-insert '"\C-x\C-y": accept-line'
    else
        bind -m emacs-standard -x '"\C-x\C-y":__aishell_noop'
        bind -m vi-insert -x '"\C-x\C-y":__aishell_noop'
    fi
}

__aishell_expand() {
    local line=${READLINE_LINE-}
    local prompt generated

    if [[ $line =~ ^[[:space:]]*ai([[:space:]]+(.*))?$ ]]; then
        prompt=${BASH_REMATCH[2]-}
        if [[ -z ${prompt//[[:space:]]/} ]]; then
            # A nested `read` corrupts Readline's pending key macro. Keep the
            # description in the real edit buffer and handle its next Enter.
            READLINE_LINE='ai '
            READLINE_POINT=${#READLINE_LINE}
            __aishell_prompting=1
            printf '\n[ai] Describe the command after "ai", then press Enter.\n' >&2
            __aishell_bind_fallback noop
            return
        fi
    elif (( READLINE_POINT != ${#READLINE_LINE} )) \
        || [[ -z ${line//[[:space:]]/} ]] \
        || __aishell_has_shell_match "$line"; then
        __aishell_bind_fallback complete
        return
    else
        prompt=$line
    fi

    __aishell_prompting=0
    printf '\n[ai] generating command...\n' >&2
    if generated=$(command ai --shell bash -- "$prompt"); then
        if [[ -n $generated ]]; then
            READLINE_LINE=$generated
            READLINE_POINT=${#READLINE_LINE}
        fi
    else
        printf '[ai] generation failed; original input kept\n' >&2
    fi
    __aishell_bind_fallback noop
}

__aishell_accept_or_expand() {
    local line=${READLINE_LINE-}

    if [[ $line =~ ^[[:space:]]*ai[[:space:]]*$ ]] \
        || (( __aishell_prompting )) \
            && [[ $line =~ ^[[:space:]]*ai([[:space:]]+(.*))?$ ]]; then
        __aishell_expand
        # Leave the generated command in Readline instead of accepting it.
        __aishell_bind_accept_fallback noop
    else
        __aishell_prompting=0
        __aishell_bind_accept_fallback accept
    fi
}

bind -m emacs-standard -x '"\C-x\C-a":__aishell_expand'
bind -m vi-insert -x '"\C-x\C-a":__aishell_expand'
bind -m emacs-standard '"\C-i":"\C-x\C-a\C-x\C-z"'
bind -m vi-insert '"\C-i":"\C-x\C-a\C-x\C-z"'
bind -m emacs-standard -x '"\C-x\C-e":__aishell_accept_or_expand'
bind -m vi-insert -x '"\C-x\C-e":__aishell_accept_or_expand'
bind -m emacs-standard '"\C-m":"\C-x\C-e\C-x\C-y"'
bind -m vi-insert '"\C-m":"\C-x\C-e\C-x\C-y"'
bind -m emacs-standard '"\C-j":"\C-x\C-e\C-x\C-y"'
bind -m vi-insert '"\C-j":"\C-x\C-e\C-x\C-y"'
"#;

const ZSH_INIT: &str = r#"__aishell_parse_request() {
    emulate -L zsh
    setopt localoptions extendedglob
    local line=${1##[[:space:]]#}

    [[ $line == ai || $line == ai[[:space:]]* ]] || return 1
    typeset -g __aishell_request=${line#ai}
    __aishell_request=${__aishell_request##[[:space:]]#}
}

__aishell_expand() {
    emulate -L zsh
    local prompt generated error_file error_text
    local -i generation_status

    if __aishell_parse_request "$BUFFER"; then
        prompt=$__aishell_request
        if [[ -z ${prompt//[[:space:]]/} ]]; then
            # Recursive ZLE sessions interact badly with transient and
            # multi-line prompts. Continue in the real edit buffer instead.
            BUFFER='ai '
            CURSOR=${#BUFFER}
            typeset -g __aishell_prompting=1
            POSTDISPLAY=$'\n[ai] Describe the command after "ai", then press Enter.'
            return
        fi
    elif (( CURSOR != ${#BUFFER} )) \
        || [[ -z ${BUFFER//[[:space:]]/} ]] \
        || __aishell_has_shell_match "$BUFFER"; then
        zle expand-or-complete
        return
    else
        prompt=$BUFFER
    fi

    typeset -g __aishell_prompting=0
    POSTDISPLAY=
    zle -R '[ai] generating command...'

    # Capture diagnostics so terminal output cannot invalidate ZLE's prompt
    # geometry. Clarifying questions are restored below the edit buffer.
    if ! error_file=$(mktemp "${TMPDIR:-/tmp}/aishell.zsh.XXXXXXXX"); then
        POSTDISPLAY=$'\n[ai] could not create a temporary diagnostics file; original input kept'
        return
    fi
    generated=$(command ai --shell zsh -- "$prompt" 2>"$error_file")
    generation_status=$?
    error_text=$(<"$error_file")
    command rm -f -- "$error_file"

    if (( generation_status == 0 )); then
        if [[ -n $generated ]]; then
            BUFFER=$generated
            CURSOR=${#BUFFER}
        elif [[ -n $error_text ]]; then
            POSTDISPLAY=$'\n'$error_text
        fi
    else
        POSTDISPLAY=$'\n'${error_text:+$error_text$'\n'}'[ai] generation failed; original input kept'
    fi
    zle -R
}

__aishell_has_shell_match() {
    emulate -L zsh
    setopt localoptions extendedglob
    local line=$1
    local first initial candidate directory basename has_arguments=0

    line=${line##[[:space:]]#}
    first=${line%%[[:space:]]*}
    [[ -z $first ]] && return 0
    [[ $line == *[[:space:]]* ]] && has_arguments=1

    initial=${first[1]}
    if [[ $initial != [[:alnum:]_] || $first == *'='* ]]; then
        return 0
    fi

    for candidate in \
        ${(k)commands} ${(k)aliases} ${(k)functions} ${(k)builtins} ${(k)reswords}; do
        if (( has_arguments )); then
            [[ $candidate == $first ]] && return 0
        else
            [[ ${candidate[1,${#first}]} == $first ]] && return 0
        fi
    done

    if (( has_arguments )); then
        [[ -e $first ]] && return 0
        return 1
    fi

    directory=${first:h}
    basename=${first:t}
    [[ -d $directory ]] || return 1
    for candidate in "$directory"/*(N) "$directory"/.*(N); do
        [[ ${${candidate:t}[1,${#basename}]} == $basename ]] && return 0
    done
    return 1
}

__aishell_accept_or_expand() {
    emulate -L zsh

    if __aishell_parse_request "$BUFFER"; then
        if [[ -z ${__aishell_request//[[:space:]]/} ]] || (( __aishell_prompting )); then
            zle __aishell_expand
            return
        fi
    fi

    typeset -g __aishell_prompting=0
    zle .accept-line
}

typeset -gi __aishell_prompting=0
typeset -g __aishell_request=
zle -N __aishell_expand
zle -N __aishell_accept_or_expand
for __aishell_keymap in emacs viins; do
    bindkey -M $__aishell_keymap '^I' __aishell_expand
    bindkey -M $__aishell_keymap '^M' __aishell_accept_or_expand
    bindkey -M $__aishell_keymap '^J' __aishell_accept_or_expand
done
unset __aishell_keymap
"#;

#[cfg(test)]
mod tests {
    use super::{Shell, init_script};

    #[test]
    fn bash_widget_passes_the_exact_prompt_as_one_argument() {
        let script = init_script(Shell::Bash);
        assert!(script.contains("command ai --shell bash -- \"$prompt\""));
        assert!(script.contains("READLINE_LINE=$generated"));
        assert!(script.contains("__aishell_has_shell_match \"$line\""));
        assert!(script.contains("compgen -A command"));
        assert!(script.contains("prompt=$line"));
        assert!(script.contains("[[ -n $generated ]]"));
        assert!(script.contains("READLINE_LINE='ai '"));
        assert!(script.contains("__aishell_prompting=1"));
        assert!(!script.contains("stty icanon echo"));
        assert!(!script.contains("IFS= read"));
        assert!(script.contains("__aishell_accept_or_expand"));
        assert!(script.contains("\"\\C-m\":\"\\C-x\\C-e\\C-x\\C-y\""));
        assert!(script.contains("\"\\C-j\":\"\\C-x\\C-e\\C-x\\C-y\""));
    }

    #[test]
    fn zsh_widget_replaces_the_complete_buffer() {
        let script = init_script(Shell::Zsh);
        assert!(script.contains("command ai --shell zsh -- \"$prompt\""));
        assert!(script.contains("BUFFER=$generated"));
        assert!(script.contains("__aishell_parse_request \"$BUFFER\""));
        assert!(script.contains("[[ $line == ai || $line == ai[[:space:]]* ]]"));
        assert!(script.contains("__aishell_has_shell_match \"$BUFFER\""));
        assert!(script.contains("${(k)commands}"));
        assert!(script.contains("prompt=$BUFFER"));
        assert!(script.contains("[[ -n $generated ]]"));
        assert!(script.contains("zle expand-or-complete"));
        assert!(script.contains("BUFFER='ai '"));
        assert!(script.contains("typeset -g __aishell_prompting=1"));
        assert!(script.contains("POSTDISPLAY=$'\\n[ai] Describe the command"));
        assert!(script.contains("2>\"$error_file\""));
        assert!(script.contains("zle -R"));
        assert!(!script.contains("zle -M"));
        assert!(!script.contains("zle recursive-edit"));
        assert!(script.contains("for __aishell_keymap in emacs viins"));
        assert!(script.contains("bindkey -M $__aishell_keymap '^M' __aishell_accept_or_expand"));
        assert!(script.contains("bindkey -M $__aishell_keymap '^J' __aishell_accept_or_expand"));
    }
}
