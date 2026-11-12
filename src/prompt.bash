# termx's built-in prompt for Git Bash, robbyrussell style. Passed in as
# PROMPT_COMMAND, so a ~/.bashrc that sets its own PROMPT_COMMAND wins.
# Inline rather than functions (env vars cannot carry those), and no $(...)
# subshells: every fork costs real time under MSYS.
__gt_s=$?
__gt_b=
__gt_d=$PWD
while [ -n "$__gt_d" ]; do
  if [ -f "$__gt_d/.git/HEAD" ]; then
    read -r __gt_h < "$__gt_d/.git/HEAD"
    case $__gt_h in
      "ref: refs/heads/"*) __gt_b=${__gt_h#ref: refs/heads/} ;;
      *) __gt_b=${__gt_h:0:7} ;;
    esac
    break
  fi
  __gt_d=${__gt_d%/*}
done
if [ $__gt_s -eq 0 ]; then PS1='\[\e]0;\w\a\e[1;32m\]➜'; else PS1='\[\e]0;\w\a\e[1;31m\]➜'; fi
PS1+='  \[\e[1;36m\]\W\[\e[0m\]'
# Single quotes: the branch is expanded once at display time and never
# re-evaluated, so a branch named $(...) cannot run anything.
[ -n "$__gt_b" ] && PS1+=' \[\e[1;34m\]git:(\[\e[31m\]$__gt_b\[\e[1;34m\])\[\e[0m\]'
PS1+=' '
