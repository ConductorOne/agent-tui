# OSC 133 (FinalTerm) shell integration for bash.
#
# Emits three markers around the user-interaction lifecycle:
#   - A: prompt start (shell about to draw its prompt)
#   - C: output start (a command just started running)
#   - D: command end (with the exit status as an optional payload)
#
# The agent-tui daemon scans PTY output for these and uses them to
# upgrade PaneState::Unknown -> Shell / Running with high confidence.
#
# Spec: https://wezterm.org/shell-integration.html

# Only do this for interactive shells.
case $- in
    *i*) ;;
    *) return ;;
esac

# Prompt-start (A) + previous-command-end (D, with exit status).
__osc133_prompt_command() {
    local exit_status=$?
    printf '\033]133;D;%d\007' "$exit_status"
    printf '\033]133;A\007'
    return $exit_status
}

# DEBUG fires before every command; emit C (output start) for real
# commands and skip our own bookkeeping.
__osc133_preexec() {
    # Skip bash-completion machinery.
    [[ -n "$COMP_LINE" ]] && return
    # Skip the prompt-command itself.
    [[ "$BASH_COMMAND" == "$PROMPT_COMMAND" ]] && return
    # Skip when invoked recursively from PROMPT_COMMAND.
    [[ "$BASH_COMMAND" == __osc133_* ]] && return
    printf '\033]133;C\007'
}

# Chain into any existing PROMPT_COMMAND.
if [[ -n "$PROMPT_COMMAND" ]]; then
    PROMPT_COMMAND="__osc133_prompt_command;${PROMPT_COMMAND}"
else
    PROMPT_COMMAND='__osc133_prompt_command'
fi

trap '__osc133_preexec' DEBUG
