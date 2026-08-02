# HadalOS build-failure capture.
#
# Sourced by /etc/portage/bashrc for every ebuild phase, so it must stay cheap
# and must never interfere with the build. Every function here returns 0
# unconditionally: a bug in the assistant's plumbing must not be able to turn a
# working emerge into a failing one, and must not mask the real failure with
# its own.
#
# Capture is deliberately dumb and synchronous — record what happened, copy the
# log somewhere durable, get out of the way. Explanation happens later, on
# demand, via `hadal explain`. Nobody wants a language model between them and
# their shell prompt at the moment a build just died.

HADALOS_SPOOL="${HADALOS_SPOOL:-/var/lib/hadalos/build-failures}"
HADALOS_LOGDIR="${HADALOS_LOGDIR:-/var/log/portage/hadalos}"

# Minimal JSON string escaping. The values written here are package names,
# phases and paths, but a log path can contain anything the admin configured,
# so it still gets escaped rather than trusted.
_hadalos_json_escape() {
    local s=$1
    s=${s//\\/\\\\}
    s=${s//\"/\\\"}
    s=${s//$'\n'/\\n}
    s=${s//$'\t'/\\t}
    s=${s//$'\r'/}
    printf '%s' "$s"
}

# Does the work and reports whether anything was recorded. Split out from the
# hook itself so that stderr can be silenced here — a failed mkdir must not
# spray errors into a build log that is already reporting a real failure —
# without also silencing the message the user is supposed to read.
_hadalos_capture() {
    {
        [[ -n ${CATEGORY:-} && -n ${PF:-} ]] || return 1

        mkdir -p "$HADALOS_SPOOL" "$HADALOS_LOGDIR" 2>/dev/null || return 1

        local stamp id log_src log_dst
        stamp=$(date -u +%Y%m%dT%H%M%SZ 2>/dev/null) || return 1
        id="${stamp}-${CATEGORY//\//_}_${PF}"

        # PORTAGE_LOG_FILE only exists when PORTAGE_LOGDIR is configured.
        # Otherwise the log lives in $T and is deleted with the build dir, so
        # it has to be copied out now or it is gone by the time anyone asks.
        log_src="${PORTAGE_LOG_FILE:-${T:-}/build.log}"
        log_dst=""
        if [[ -r $log_src ]]; then
            log_dst="$HADALOS_LOGDIR/${id}.log"
            # Tail rather than whole: a failed webkit build log can be
            # hundreds of MB, and the error is at the end. The broker caps
            # its own read too, but there is no reason to store what nobody
            # will look at.
            tail -c 2097152 "$log_src" > "$log_dst" 2>/dev/null || log_dst=""
            [[ -n $log_dst ]] && chmod 0644 "$log_dst" 2>/dev/null
        fi

        umask 022
        cat > "$HADALOS_SPOOL/${id}.json" <<EOF
{
  "package": "$(_hadalos_json_escape "${CATEGORY}/${PF}")",
  "category": "$(_hadalos_json_escape "${CATEGORY}")",
  "phase": "$(_hadalos_json_escape "${EBUILD_PHASE:-unknown}")",
  "log": "$(_hadalos_json_escape "${log_dst}")",
  "original_log": "$(_hadalos_json_escape "${log_src}")",
  "recorded": "$(_hadalos_json_escape "${stamp}")",
  "use": "$(_hadalos_json_escape "${USE:-}")"
}
EOF
    } 2>/dev/null || return 1

    return 0
}

hadalos_record_build_failure() {
    if _hadalos_capture; then
        # A few lines at the end of a wall of build output. No colour: this is
        # as likely to be scrolling into a log file as onto a terminal.
        echo
        echo " * HadalOS recorded this failure. Run 'hadal explain' for an analysis."
        echo
    fi

    # Never change the outcome of the build. A bug in the assistant's plumbing
    # must not turn a failing emerge into a differently-failing one, and must
    # not mask the real failure with its own.
    return 0
}

# Portage calls these when an ebuild dies. This is the supported mechanism —
# trapping ERR or EXIT inside bashrc would fire on phases that merely returned
# non-zero without failing.
EBUILD_DEATH_HOOKS+=" hadalos_record_build_failure"
