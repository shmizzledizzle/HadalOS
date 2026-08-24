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

# ── System identity, after a baselayout merge ─────────────────────────────
#
# sys-apps/baselayout ships /etc/os-release as a symlink to ../usr/lib/os-release.
# sys-apps/hadalos-release replaces it with a real file, which is what
# os-release(5) precedence is for. A baselayout upgrade puts its symlink back,
# and the system silently returns to identifying as Gentoo.
#
# Observed on this machine: baselayout-2.18-r1 merged 2026-08-19 16:49:12, and
# /etc/os-release has been a symlink to Gentoo's copy ever since. Nothing
# reported it. `emerge` does not read os-release, so nothing broke — the only
# visible symptom is neofetch saying "Gentoo Linux" on a HadalOS install, which
# reads as a neofetch quirk rather than as the identity having been reverted.
#
# scripts/test-overlay.sh already asserts the end state, but only when someone
# runs it. This says so at the moment it happens, to the person who caused it.
# It warns and does not repair: pkg_preinst in hadalos-release is where the
# removal belongs, and a bashrc hook that rewrites /etc during someone else's
# merge is the kind of surprise this tree does not want.
_hadalos_check_identity() {
    [[ ${CATEGORY}/${PN} == sys-apps/baselayout ]] || return 0
    # Only meaningful where the identity was actually installed. compgen rather
    # than `[[ -d ... ]]`, which does not expand globs and would be true for a
    # literal path with a `*` in it — that is, never.
    compgen -G "${EROOT:-}/var/db/pkg/sys-apps/hadalos-release-*" >/dev/null 2>&1 || return 0
    [[ -L ${EROOT:-}/etc/os-release ]] || return 0

    echo
    echo " * HadalOS: baselayout has restored its /etc/os-release symlink."
    echo " *   This system now identifies as Gentoo again. Restore it with:"
    echo " *     emerge -C sys-apps/hadalos-release"
    echo " *     emerge -1 sys-apps/baselayout"
    echo " *     emerge    sys-apps/hadalos-release"
    echo " *   See the hadalos-release ebuild for why a plain -1 is not enough."
    echo
    return 0
}

# post_pkg_postinst rather than a death hook: this is about a merge that
# succeeded. Wrapped so that an existing definition from another bashrc.d
# snippet is not clobbered — this file is one of several and is not entitled to
# own the phase.
#
# Registered at most once, for the reason the death hook above is: this file is
# sourced for every ebuild phase and Portage carries the environment between
# them, so an unguarded wrapper chains onto its own previous wrapper and the
# warning is printed once per phase already run. The flag is exported for the
# same reason — the phases are separate bash processes, and a plain shell
# variable does not survive between them.
if [[ -z ${HADALOS_IDENTITY_HOOK_REGISTERED:-} ]]; then
export HADALOS_IDENTITY_HOOK_REGISTERED=1
if declare -F post_pkg_postinst >/dev/null 2>&1; then
    eval "_hadalos_prev_post_pkg_postinst() { $(declare -f post_pkg_postinst | tail -n +2) }"
    post_pkg_postinst() {
        _hadalos_prev_post_pkg_postinst "$@"
        _hadalos_check_identity
        return 0
    }
else
    post_pkg_postinst() {
        _hadalos_check_identity
        return 0
    }
fi
fi
