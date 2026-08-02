#!/bin/bash
# integration-test.sh — exercise hadal-brokerd against a real D-Bus and a real
# polkit.
#
# The unit tests prove the validators reject bad input. They cannot prove the
# daemon claims its bus name, that polkit recognises the shipped policy, or
# that the fail-closed startup check actually fails closed. Those need a live
# system bus, which is what this uses.
#
# Run as root on a systemd machine (WSL Debian is fine):
#   bash scripts/integration-test.sh
#
# Needs a release binary; run scripts/wsl-verify.sh first.

set -uo pipefail

BUILD="${HADALOS_BUILD:-$HOME/hadalos-build}"
BIN="$BUILD/src/hadal-brokerd/target/release/hadal-brokerd"
POLICY_SRC="$BUILD/policy/org.hadal.broker.policy"
DBUS_SRC="$BUILD/dbus/org.hadal.Broker1.conf"

POLICY_DST=/usr/share/polkit-1/actions/org.hadal.broker.policy
DBUS_DST=/usr/share/dbus-1/system.d/org.hadal.Broker1.conf

SERVICE=org.hadal.Broker1
OBJECT=/org/hadal/Broker1

pass=0
fail=0

step()  { printf '\n\033[1;36m══ %s\033[0m\n' "$*"; }
ok()    { printf '\033[32m  PASS\033[0m %s\n' "$*"; pass=$((pass+1)); }
bad()   { printf '\033[31m  FAIL\033[0m %s\n' "$*"; fail=$((fail+1)); }
note()  { printf '       %s\n' "$*"; }

cleanup() {
    [[ -n ${BROKER_PID:-} ]] && kill "$BROKER_PID" 2>/dev/null
    rm -f "$POLICY_DST" "$DBUS_DST"
    systemctl reload dbus 2>/dev/null
}
trap cleanup EXIT

[[ -x $BIN ]] || { echo "no binary at $BIN — run scripts/wsl-verify.sh first"; exit 1; }

# ─────────────────────────────────────────────────────────────────────────
step "preflight"
systemctl is-active --quiet dbus && ok "system bus is running" || { bad "no system bus"; exit 1; }
rm -f "$POLICY_DST" "$DBUS_DST"
systemctl reload dbus 2>/dev/null
sleep 1

# ─────────────────────────────────────────────────────────────────────────
# The most important test here. A broker whose policy file is missing would
# otherwise start happily and then deny every capability with no explanation.
step "fail-closed: refuses to start without its polkit policy"

out="$("$BIN" 2>&1 <&- & pid=$!; sleep 4; kill $pid 2>/dev/null; wait $pid 2>/dev/null; true)"
if grep -q "polkit policy not installed" <<<"$out"; then
    ok "refused to start, and said why"
    note "$(grep -o 'missing actions:.*' <<<"$out" | head -c 120)..."
else
    bad "started (or failed for the wrong reason) without its policy"
    note "$(head -3 <<<"$out")"
fi

# ─────────────────────────────────────────────────────────────────────────
step "installing policy and bus configuration"
install -Dm644 "$POLICY_SRC" "$POLICY_DST" && ok "polkit policy installed"
install -Dm644 "$DBUS_SRC" "$DBUS_DST" && ok "bus policy installed"
systemctl reload dbus
sleep 1

# polkit reads action files at startup.
systemctl restart polkit 2>/dev/null || systemctl start polkit 2>/dev/null
sleep 2
systemctl is-active --quiet polkit && ok "polkit is running" || bad "polkit not running"

# Does polkit actually parse our file? A malformed policy is silently ignored.
n_actions=$(busctl call org.freedesktop.PolicyKit1 /org/freedesktop/PolicyKit1/Authority \
    org.freedesktop.PolicyKit1.Authority EnumerateActions s "" 2>/dev/null \
    | grep -o 'org\.hadal\.broker\.[a-z-]*' | sort -u | wc -l)
if [[ $n_actions -eq 10 ]]; then
    ok "polkit parsed all 10 capability actions"
else
    bad "polkit sees $n_actions of 10 capability actions"
fi

# ─────────────────────────────────────────────────────────────────────────
step "starting broker"
"$BIN" >/tmp/brokerd.log 2>&1 &
BROKER_PID=$!
sleep 4

if kill -0 "$BROKER_PID" 2>/dev/null; then
    ok "broker is running (pid $BROKER_PID)"
else
    bad "broker exited during startup"
    cat /tmp/brokerd.log
    exit 1
fi

grep -q "polkit policy verified" /tmp/brokerd.log \
    && ok "startup policy verification passed" \
    || bad "no policy verification in log"

busctl status "$SERVICE" >/dev/null 2>&1 \
    && ok "claimed the well-known name $SERVICE" \
    || bad "did not claim $SERVICE"

# ─────────────────────────────────────────────────────────────────────────
step "introspection"
intro="$(busctl introspect "$SERVICE" "$OBJECT" 2>&1)"
for member in CreateSession AvailableCapabilities AvailableTiers Ready Version; do
    grep -q "$member" <<<"$intro" && ok "exposes $member" || bad "missing $member"
done

# ─────────────────────────────────────────────────────────────────────────
step "capability discovery"
caps="$(busctl call "$SERVICE" "$OBJECT" "$SERVICE" AvailableCapabilities 2>&1)"
n=$(grep -o '"[a-z-]*" "\(allow\|auth\|deny\)"' <<<"$caps" | wc -l)
[[ $n -eq 10 ]] && ok "reported 10 capabilities" || { bad "reported $n capabilities"; note "$caps"; }

grep -q '"emerge-apply" "auth"' <<<"$caps" \
    && ok "emerge-apply is auth-gated, not allow" \
    || bad "emerge-apply has the wrong disposition"
grep -q '"network-lookup" "deny"' <<<"$caps" \
    && ok "network-lookup denied by default" \
    || bad "network-lookup is not denied"
grep -q '"read-journal" "allow"' <<<"$caps" \
    && ok "read-journal permitted for an active session" \
    || bad "read-journal has the wrong disposition"

# ─────────────────────────────────────────────────────────────────────────
step "session lifecycle"
sess="$(busctl call "$SERVICE" "$OBJECT" "$SERVICE" CreateSession 'a{sv}' 2 \
        tier s auto surface s integration-test 2>&1)"
path="$(grep -oE '/org/hadal/Broker1/session/[0-9]+' <<<"$sess" | head -1)"

if [[ -n $path ]]; then
    ok "created session at $path"
else
    bad "CreateSession failed"
    note "$sess"
fi

if [[ -n $path ]]; then
    sintro="$(busctl introspect "$SERVICE" "$path" 2>&1)"
    for member in Ask Cancel Execute Discard Close Delta Finished ActionProposed; do
        grep -q "$member" <<<"$sintro" && ok "session exposes $member" || bad "session missing $member"
    done

    owner="$(busctl get-property "$SERVICE" "$path" org.hadal.Session1 Owner 2>&1)"
    grep -q "u 0" <<<"$owner" \
        && ok "Owner resolved from the bus, not the caller (uid 0)" \
        || { bad "Owner wrong: $owner"; }

    # ── the token check, without a model in the loop ──
    # A caller cannot conjure a token; the broker mints them only after
    # validating a proposal. Executing a fabricated one must fail.
    ex="$(busctl call "$SERVICE" "$path" org.hadal.Session1 Execute s \
          "0000000000000000000000000000000000000000000000000000000000000000" 2>&1)"
    if grep -qi "no such pending proposal" <<<"$ex"; then
        ok "fabricated token rejected"
    else
        bad "fabricated token was not rejected cleanly"
        note "$ex"
    fi

    busctl call "$SERVICE" "$path" org.hadal.Session1 Close >/dev/null 2>&1 \
        && ok "session closed" || bad "Close failed"
fi

# ─────────────────────────────────────────────────────────────────────────
step "degraded model host"
# hadald is not running here. The broker must report that honestly rather
# than pretending to be ready.
ready="$(busctl get-property "$SERVICE" "$OBJECT" "$SERVICE" Ready 2>&1)"
grep -q "b false" <<<"$ready" \
    && ok "Ready reports false with no hadald" \
    || bad "Ready is wrong with no hadald: $ready"

# ─────────────────────────────────────────────────────────────────────────
printf '\n\033[1m══ %d passed, %d failed\033[0m\n' "$pass" "$fail"
[[ $fail -eq 0 ]] || { printf '\n--- brokerd log ---\n'; cat /tmp/brokerd.log; }
exit $(( fail > 0 ? 1 : 0 ))
