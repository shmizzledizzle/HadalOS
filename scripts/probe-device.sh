#!/usr/bin/env bash
# Push hadal-probe to the device and run the validator corpus on it.
#
# This is the phase 1 verification step, and the point is narrow but real: the
# validators must behave identically on aarch64/bionic as they do on
# x86_64/glibc. The desktop project makes the same move with wsl-verify.sh,
# refusing to trust the authoring machine's idea of what its own code does.
#
# Read-only with respect to the device. It pushes one static binary to
# /data/local/tmp, runs it, and deletes it. It does not need root, does not
# need an unlocked bootloader, and touches nothing outside that directory.
set -euo pipefail

REMOTE_DIR="${REMOTE_DIR:-/data/local/tmp/hadal}"
TARGET=aarch64-linux-android
PROFILE="${PROFILE:-release}"
BIN="$(dirname "$0")/../src/hadal-brokerd/target/$TARGET/$PROFILE/hadal-probe"

if [[ ! -f $BIN ]]; then
    echo "error: $BIN not built — run scripts/build-android.sh first" >&2
    exit 1
fi

state=$(adb devices | awk 'NR>1 && NF {print $2; exit}')
case "${state:-none}" in
    device) ;;
    unauthorized)
        echo "error: device is unauthorized" >&2
        echo "       unlock the phone and accept the USB debugging prompt." >&2
        echo "       if no prompt appears: Developer options ->" >&2
        echo "       Revoke USB debugging authorizations, then replug." >&2
        exit 1
        ;;
    none)
        echo "error: no device. check the cable and 'adb devices'." >&2
        exit 1
        ;;
    *)
        echo "error: device in unexpected state: $state" >&2
        exit 1
        ;;
esac

echo "=== device ==="
for p in ro.product.model ro.build.version.release ro.build.version.sdk \
         ro.product.cpu.abi ro.build.flavor; do
    printf '  %-28s %s\n' "$p" "$(adb shell getprop $p | tr -d '\r')"
done

echo
echo "=== pushing ==="
adb shell "mkdir -p $REMOTE_DIR"
adb push "$BIN" "$REMOTE_DIR/hadal-probe" >/dev/null
adb shell "chmod 755 $REMOTE_DIR/hadal-probe"
echo "  -> $REMOTE_DIR/hadal-probe"

echo
echo "=== capability table (on device) ==="
adb shell "$REMOTE_DIR/hadal-probe capabilities" | tr -d '\r'

echo
echo "=== selftest (on device) ==="
set +e
adb shell "$REMOTE_DIR/hadal-probe selftest; echo EXIT=\$?" | tr -d '\r' | tee /tmp/hadal-probe-out.$$
rc=$(awk -F= '/^EXIT=/{print $2}' /tmp/hadal-probe-out.$$)
rm -f /tmp/hadal-probe-out.$$
set -e

echo
echo "=== cleanup ==="
adb shell "rm -rf $REMOTE_DIR"
echo "  removed $REMOTE_DIR"

if [[ ${rc:-1} -ne 0 ]]; then
    echo
    echo "SELFTEST FAILED on device (exit $rc)" >&2
    exit 1
fi
echo
echo "selftest passed on device"
