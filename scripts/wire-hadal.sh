#!/usr/bin/env bash
# Wire hadald to hadal-brokerd so `hadal ask` works end to end on this machine.
#
#   scripts/wire-hadal.sh install   # needs root: policy files + dbus reload
#   scripts/wire-hadal.sh run       # needs root: starts hadald + broker
#   scripts/wire-hadal.sh status    # no root
#
# Topology for a manual bring-up. NOT the shipped one — the systemd units put
# hadald in a network namespace and have the broker join it. Here both sit in
# the host namespace and talk over host loopback, which is the same wire
# protocol with weaker isolation.
#
#   hadal (you)  ──system D-Bus──▶  hadal-brokerd (root)
#                                       │ plaintext HTTP 127.0.0.1:11434
#                                       ▼
#                                    hadald (you) ──TLS──▶ upstream
set -uo pipefail

HERE="$(cd "$(dirname "$0")/.." && pwd)"
OS="$HERE/HadalOS"
BIN="$OS/src/target/debug"
HADALD="$HERE/src/hadald/target/debug/hadald"
MODEL="${HADAL_MODEL:-nvidia/llama-3.3-nemotron-super-49b-v1.5}"
KEY="${HADAL_KEY:-$HOME/.config/hadal/upstream.key}"
RUNDIR="${XDG_RUNTIME_DIR:-/tmp}/hadal-wire"

say()  { printf '\033[1m%s\033[0m\n' "$*"; }
have() { [[ -x $1 ]] || { echo "missing: $1 — run cargo build first" >&2; exit 2; }; }

case "${1:-status}" in

install)
    [[ $EUID -eq 0 ]] || { echo "install needs root: sudo $0 install" >&2; exit 1; }
    say "installing D-Bus system policy"
    install -m 0644 "$OS/dbus/org.hadal.Broker1.conf" /etc/dbus-1/system.d/
    say "installing polkit actions"
    install -m 0644 "$OS/policy/org.hadal.broker.policy" /usr/share/polkit-1/actions/
    # The broker refuses to start if polkit cannot enumerate its actions —
    # polkit rescans /usr/share/polkit-1/actions on its own, but dbus needs a
    # reload before it will let anyone own the name.
    say "reloading D-Bus configuration"
    if command -v systemctl >/dev/null; then
        systemctl reload dbus 2>/dev/null || busctl call org.freedesktop.DBus \
            /org/freedesktop/DBus org.freedesktop.DBus ReloadConfig 2>/dev/null || true
    fi
    say "done — now: sudo $0 run"
    ;;

run)
    [[ $EUID -eq 0 ]] || { echo "run needs root (the broker owns a system bus name): sudo -E $0 run" >&2; exit 1; }
    have "$HADALD"; have "$BIN/hadal-brokerd"
    [[ -f /etc/dbus-1/system.d/org.hadal.Broker1.conf ]] || {
        echo "D-Bus policy not installed — run: sudo $0 install" >&2; exit 1; }

    # hadald runs as the invoking user, not root: it holds the API key and no
    # privilege, and root would let it read a key file it has no business
    # reading. SUDO_USER is who actually asked.
    OWNER="${SUDO_USER:-$USER}"
    mkdir -p "$RUNDIR"; chown "$OWNER" "$RUNDIR"

    say "starting hadald as $OWNER (model: $MODEL)"
    runuser -u "$OWNER" -- "$HADALD" --serve --model "$MODEL" \
        --key-file "$KEY" --egress-log "$RUNDIR/egress.log" \
        > "$RUNDIR/hadald.log" 2>&1 &
    echo $! > "$RUNDIR/hadald.pid"

    for _ in $(seq 1 100); do
        (exec 3<>/dev/tcp/127.0.0.1/11434) 2>/dev/null && break
        sleep 0.1
    done
    (exec 3<>/dev/tcp/127.0.0.1/11434) 2>/dev/null || {
        echo "hadald did not come up:" >&2; tail -5 "$RUNDIR/hadald.log" >&2; exit 1; }

    say "starting hadal-brokerd as root"
    HADAL_ENDPOINT="http://127.0.0.1:11434" "$BIN/hadal-brokerd" \
        > "$RUNDIR/brokerd.log" 2>&1 &
    echo $! > "$RUNDIR/brokerd.pid"
    sleep 2

    if ! busctl --system list 2>/dev/null | grep -q org.hadal.Broker1; then
        echo "broker did not take the bus name:" >&2
        tail -15 "$RUNDIR/brokerd.log" >&2
        exit 1
    fi

    say "up. as your normal user:"
    echo "    $BIN/hadal status"
    echo "    $BIN/hadal ask 'why did my last kernel install fail?'"
    echo
    echo "  logs:  $RUNDIR/{hadald,brokerd}.log"
    echo "  egress: $RUNDIR/egress.log"
    echo "  stop:  sudo kill \$(cat $RUNDIR/*.pid)"
    ;;

status)
    printf '  %-34s %s\n' "dbus policy" \
        "$([ -f /etc/dbus-1/system.d/org.hadal.Broker1.conf ] && echo installed || echo MISSING)"
    printf '  %-34s %s\n' "polkit actions" \
        "$([ -f /usr/share/polkit-1/actions/org.hadal.broker.policy ] && echo installed || echo MISSING)"
    printf '  %-34s %s\n' "hadald :11434" \
        "$((exec 3<>/dev/tcp/127.0.0.1/11434) 2>/dev/null && echo up || echo down)"
    printf '  %-34s %s\n' "org.hadal.Broker1 on system bus" \
        "$(busctl --system list 2>/dev/null | grep -q org.hadal.Broker1 && echo owned || echo absent)"
    # An authentication agent registers with polkitd over the *system* bus and
    # owns no well-known name of its own, so look for the process. Checking a
    # bus name here reported "none" while polkit-kde-authentication-agent-1
    # was running the whole time.
    printf '  %-34s %s\n' "polkitd" \
        "$(pgrep -x polkitd >/dev/null && echo running || echo 'NOT RUNNING — broker will refuse to start')"
    printf '  %-34s %s\n' "polkit auth agent" \
        "$(pgrep -f 'polkit.*authentication-agent|polkit-kde|polkit-gnome' >/dev/null \
           && echo present \
           || echo 'none — Mutate actions cannot prompt; use pkttyagent')"
    ;;

*)
    echo "usage: $0 {install|run|status}" >&2; exit 2 ;;
esac
