#!/bin/sh
# Called by the interactive shell's codex alias. Resolve the real CLI on its
# current PATH (nvm, Homebrew, etc.). Never interpret the user's arguments as shell.
if [ -z "${MAITERM_TAB_ID:-}" ]; then exec codex "$@"; fi

interactive_launch() {
    while [ "$#" -gt 0 ]; do
        case "$1" in
            --no-daemon|--remote|--remote=*|--help|-h|--version|-V) return 1 ;;
            --) return 0 ;; # everything after this is a literal prompt
            -c|--config|--enable|--disable|-i|--image|-m|--model|--local-provider|-p|--profile|-s|--sandbox|-C|--cd|--add-dir|-a|--ask-for-approval|--remote-auth-token-env)
                shift
                [ "$#" -gt 0 ] || return 1 ;;
            -*) ;;
            agents|exec|e|review|login|logout|mcp|plugin|app-server|remote-control|app|completion|update|doctor|sandbox|debug|apply|a|queue|archive|delete|migrate-rollouts|migrate-nfs-db|unarchive|cloud|cloud-tasks|exec-server|features|help|git-credential|tcp-tunnel|execpolicy|responses-api-proxy)
                return 1 ;;
            *) # prompt, resume or fork. Global flags can also follow the verb.
                for arg do
                    case "$arg" in
                        --) break ;;
                        --remote|--remote=*|--no-daemon|--help|-h|--version|-V) return 1 ;;
                    esac
                done
                return 0 ;;
        esac
        shift
    done
    return 0
}

# Probe this executable at launch. A package upgrade or PATH change between
# commands must take effect even in an already-open tab.
if interactive_launch "$@" && command codex --help 2>/dev/null | grep -Eq '^[[:space:]]+--no-daemon([[:space:]]|$)'; then
    set -- --no-daemon "$@"
fi
exec codex "$@"
