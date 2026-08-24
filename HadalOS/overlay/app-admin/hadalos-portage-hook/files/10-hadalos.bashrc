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
    HADALOS_CAPTURE_ERR=""
    {
        [[ -n ${CATEGORY:-} && -n ${PF:-} ]] || {
            HADALOS_CAPTURE_ERR="no CATEGORY/PF in the environment"
            return 1
        }

        # EBUILD_DEATH_HOOKS run *inside the sandbox* — the caller is
        # misc-functions.sh die_hooks — so every write outside the build tree is
        # denied unless it is declared first. Without these two lines the spool
        # and the log copy both fail with
        #
        #   ACCESS DENIED  open_wr_creat: /var/lib/hadalos/build-failures/...
        #
        # and, because this function silences stderr, the only trace is a
        # sandbox summary nobody reads. Found 2026-08-24 on the first real build
        # failure this hook ever saw: sci-ml/llama-cpp, which died on a file
        # collision and was not recorded.
        #
        # addwrite is defined in ebuild.sh and is present here — misc-functions.sh
        # calls it itself for the suid scan — but it is guarded anyway, because a
        # hook that dies on an undefined function would violate the one rule this
        # file has.
        if declare -F addwrite >/dev/null 2>&1; then
            addwrite "$HADALOS_SPOOL"
            addwrite "$HADALOS_LOGDIR"
        fi

        mkdir -p "$HADALOS_SPOOL" "$HADALOS_LOGDIR" 2>/dev/null || {
            HADALOS_CAPTURE_ERR="cannot create $HADALOS_SPOOL or $HADALOS_LOGDIR"
            return 1
        }

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
        cat > "$HADALOS_SPOOL/${id}.json" 2>/dev/null <<EOF
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
    } 2>/dev/null || {
        [[ -n ${HADALOS_CAPTURE_ERR} ]] || HADALOS_CAPTURE_ERR="write to $HADALOS_SPOOL failed"
        return 1
    }

    [[ -s "$HADALOS_SPOOL/${id}.json" ]] || {
        # The record is the product. An empty or missing file is a failure even
        # though every command above returned 0 — this is the assertion that
        # would have caught the sandbox denial on the day it was introduced.
        HADALOS_CAPTURE_ERR="no record written to $HADALOS_SPOOL"
        return 1
    }

    return 0
}

hadalos_record_build_failure() {
    if _hadalos_capture; then
        # A few lines at the end of a wall of build output. No colour: this is
        # as likely to be scrolling into a log file as onto a terminal.
        echo
        echo " * HadalOS recorded this failure. Run 'hadal explain' for an analysis."
        echo
    else
        # Say so. The original version returned silently on every failure path,
        # which is why a sandbox denial went unnoticed until someone read a
        # sandbox summary by accident — and `hadal explain` answering "no
        # recorded failure" after a failed build is indistinguishable from
        # there having been no failure.
        #
        # One line, no diagnostics beyond the reason. The build already has a
        # real error to report and this is not it.
        echo
        echo " * HadalOS could NOT record this failure: ${HADALOS_CAPTURE_ERR:-unknown}"
        echo " *   'hadal explain' will not see it. This is a bug in"
        echo " *   app-admin/hadalos-portage-hook, not in the package above."
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
#
# Registered at most once. This file is sourced for *every* ebuild phase and
# Portage carries the environment between them, so a bare append accumulates:
# by the install phase the hook is registered seven times and runs seven times
# on a single failure.
#
# That was always true and was invisible while the hook was silent — the
# earlier sandbox denials appeared seven times each and read as noise. Making
# the failure path speak is what surfaced it, which is the argument for having
# done so.
case " ${EBUILD_DEATH_HOOKS} " in
    *" hadalos_record_build_failure "*) ;;
    *) EBUILD_DEATH_HOOKS+=" hadalos_record_build_failure" ;;
esac
