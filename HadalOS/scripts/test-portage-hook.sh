#!/bin/bash
# test-portage-hook.sh — exercise the Portage death hook without Portage.
#
# The hook is a bash function Portage calls via EBUILD_DEATH_HOOKS, so it can
# be driven directly by setting the same variables Portage would. That covers
# everything except Portage actually invoking it, which the ebuild's placement
# handles.
#
# The property that matters most here is the last one: the hook must never
# change the outcome of a build. An assistant that turns a failing emerge into
# a differently-failing emerge is worse than no assistant.

set -uo pipefail

HOOK="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)/overlay/app-admin/hadalos-portage-hook/files/10-hadalos.bashrc"
TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

pass=0; fail=0
ok()  { printf '\033[32m  PASS\033[0m %s\n' "$*"; pass=$((pass+1)); }
bad() { printf '\033[31m  FAIL\033[0m %s\n' "$*"; fail=$((fail+1)); }

export HADALOS_SPOOL="$TMP/spool"
export HADALOS_LOGDIR="$TMP/logs"

# Portage's ebuild environment, as the hook would see it.
export CATEGORY="sys-boot"
export PF="limine-12.2.0"
export EBUILD_PHASE="compile"
export T="$TMP/temp"
export USE="bios uefi"
mkdir -p "$T"

# A build log with the error at the end, where a real one has it.
{
    for i in $(seq 1 2000); do echo "make[2]: building object $i"; done
    echo "limine.c:42:7: error: 'FOO' undeclared (first use in this function)"
    echo "make: *** [Makefile:17: limine.o] Error 1"
} > "$T/build.log"

EBUILD_DEATH_HOOKS=""
# shellcheck disable=SC1090
source "$HOOK"

printf '\n\033[1;36m══ portage death hook\033[0m\n'

[[ $EBUILD_DEATH_HOOKS == *hadalos_record_build_failure* ]] \
    && ok "registered itself in EBUILD_DEATH_HOOKS" \
    || bad "did not register in EBUILD_DEATH_HOOKS"

out="$(hadalos_record_build_failure 2>&1)"
rc=$?

[[ $rc -eq 0 ]] && ok "returned 0" || bad "returned $rc"
grep -q "hadal explain" <<<"$out" && ok "told the user how to follow up" || bad "no follow-up hint"

record="$(find "$HADALOS_SPOOL" -name '*.json' | head -1)"
if [[ -n $record ]]; then
    ok "wrote a spool record"
    if python3 -c "import json,sys; json.load(open(sys.argv[1]))" "$record" 2>/dev/null; then
        ok "record is valid JSON"
        pkg="$(python3 -c "import json,sys; print(json.load(open(sys.argv[1]))['package'])" "$record")"
        [[ $pkg == "sys-boot/limine-12.2.0" ]] && ok "package recorded correctly" || bad "package wrong: $pkg"
        phase="$(python3 -c "import json,sys; print(json.load(open(sys.argv[1]))['phase'])" "$record")"
        [[ $phase == "compile" ]] && ok "phase recorded correctly" || bad "phase wrong: $phase"
        logpath="$(python3 -c "import json,sys; print(json.load(open(sys.argv[1]))['log'])" "$record")"
        [[ -r $logpath ]] && ok "captured log exists at the recorded path" || bad "log path unreadable: $logpath"
        # The error is what the model needs; a head-truncated log is useless.
        grep -q "error: 'FOO' undeclared" "$logpath" 2>/dev/null \
            && ok "captured log kept the tail, where the error is" \
            || bad "captured log lost the error"
    else
        bad "record is not valid JSON"
        cat "$record"
    fi
else
    bad "no spool record written"
fi

# ── the hook must be harmless ──
printf '\n\033[1;36m══ must never break a build\033[0m\n'

# Unwritable spool: a read-only or full filesystem must not propagate.
(
    export HADALOS_SPOOL=/proc/definitely/not/writable
    export HADALOS_LOGDIR=/proc/definitely/not/writable
    hadalos_record_build_failure >/dev/null 2>&1
)
[[ $? -eq 0 ]] && ok "survives an unwritable spool directory" || bad "propagated a spool failure"

# Missing Portage variables: the hook may be sourced outside a build.
(
    unset CATEGORY PF
    hadalos_record_build_failure >/dev/null 2>&1
)
[[ $? -eq 0 ]] && ok "survives missing Portage variables" || bad "failed without CATEGORY/PF"

# No log at all.
(
    export T="$TMP/nonexistent"
    unset PORTAGE_LOG_FILE
    hadalos_record_build_failure >/dev/null 2>&1
)
[[ $? -eq 0 ]] && ok "survives a missing build log" || bad "failed with no log"

# Hostile package name. Portage would never produce this, but the hook writes
# JSON by hand, so it must not be forgeable into a broken record.
(
    export PF='evil","injected":"yes'
    hadalos_record_build_failure >/dev/null 2>&1
)
evil="$(grep -rl 'injected' "$HADALOS_SPOOL" 2>/dev/null | head -1)"
if [[ -n $evil ]]; then
    if python3 -c "
import json,sys
d=json.load(open(sys.argv[1]))
sys.exit(0 if 'injected' not in d else 1)" "$evil" 2>/dev/null; then
        ok "quotes in a package name are escaped, not structural"
    else
        bad "package name broke out into a new JSON field"
    fi
else
    ok "hostile package name produced no forged record"
fi

printf '\n\033[1m══ %d passed, %d failed\033[0m\n' "$pass" "$fail"
exit $(( fail > 0 ? 1 : 0 ))
