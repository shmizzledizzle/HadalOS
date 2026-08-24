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

# ── registration must be idempotent ──
printf '\n\033[1;36m══ registration\033[0m\n'

# /etc/portage/bashrc.d is sourced for every phase and Portage carries the
# environment between them, so a bare `EBUILD_DEATH_HOOKS+=` accumulates. By
# the install phase the hook was registered seven times and ran seven times on
# one failure — visible in the 2026-08-24 llama-cpp merge as the same message
# printed seven times, and before that as seven identical ACCESS DENIED lines
# that read as noise.
for _ in 1 2 3 4 5 6 7; do source "$HOOK"; done
count=$(printf '%s' " $EBUILD_DEATH_HOOKS " | grep -o 'hadalos_record_build_failure' | wc -l)
if [[ $count -eq 1 ]]; then
    ok "registers exactly once when sourced repeatedly"
else
    bad "registered $count times after 7 sourcings — will fire $count times per failure"
fi

# ── the sandbox, which is where this hook actually failed ──
printf '\n\033[1;36m══ sandbox declaration\033[0m\n'

# EBUILD_DEATH_HOOKS run inside the sandbox, so the spool and log directories
# have to be declared writable or every write is denied. This is the regression
# test for 2026-08-24: sci-ml/llama-cpp died on a file collision and the hook
# recorded nothing, because addwrite was never called. The build output showed
# only an ACCESS DENIED line in a sandbox summary.
addwrite_calls="$TMP/addwrite.calls"
: > "$addwrite_calls"
(
    # Stub the sandbox helper the way Portage defines it, and record the paths.
    addwrite() { printf '%s\n' "$@" >> "$addwrite_calls"; }
    export -f addwrite 2>/dev/null || true
    hadalos_record_build_failure >/dev/null 2>&1
)
if grep -qF "$HADALOS_SPOOL" "$addwrite_calls" 2>/dev/null \
   && grep -qF "$HADALOS_LOGDIR" "$addwrite_calls" 2>/dev/null; then
    ok "declares the spool and log directories writable to the sandbox"
else
    bad "did not addwrite the spool/log directories — sandbox will deny the write"
fi

# ── a failure to record must be visible ──
printf '\n\033[1;36m══ failure to record is reported\033[0m\n'

# The original hook returned silently on every failure path. That is why the
# sandbox denial went unnoticed: `hadal explain` finding no recorded failure is
# indistinguishable from there having been no failure. Staying silent is a
# worse bug than the denial it hid.
out=$(
    export HADALOS_SPOOL=/proc/definitely/not/writable
    export HADALOS_LOGDIR=/proc/definitely/not/writable
    hadalos_record_build_failure 2>&1
)
if printf '%s' "$out" | grep -qi 'could NOT record'; then
    ok "says so when it cannot record"
else
    bad "failed silently — the class of bug this hook exists to avoid"
fi

# ...and must not claim success in the same breath.
if printf '%s' "$out" | grep -qi 'recorded this failure'; then
    bad "reported both success and failure"
else
    ok "does not also claim success"
fi

# An empty record is a failure even if every command returned 0. The product is
# the record, not the exit status.
(
    export HADALOS_SPOOL="$TMP/emptyspool"
    mkdir -p "$HADALOS_SPOOL"
    # Make the spool a directory that swallows writes: a path that exists but
    # where the record file cannot be created.
    chmod 500 "$HADALOS_SPOOL"
    out2=$(hadalos_record_build_failure 2>&1)
    chmod 700 "$HADALOS_SPOOL"
    printf '%s' "$out2" | grep -qi 'could NOT record'
)
[[ $? -eq 0 ]] && ok "an unwritten record counts as failure, not success" \
              || bad "reported success without writing a record"

printf '\n\033[1m══ %d passed, %d failed\033[0m\n' "$pass" "$fail"
exit $(( fail > 0 ? 1 : 0 ))
