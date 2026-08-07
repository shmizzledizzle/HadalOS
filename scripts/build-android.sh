#!/usr/bin/env bash
# Cross-compile the broker for the device.
#
# Gentoo's dev-lang/rust ships std only for the host, and ::gentoo has no
# android-ndk, so both halves of this toolchain are user-local and neither
# needs root:
#
#   NDK     ~/Android/android-ndk-r27d   (unpacked tarball from dl.google.com)
#   rustup  ~/.cargo, ~/.rustup          (installed --no-modify-path)
#
# Because rustup was installed without touching the shell profile, the system
# cargo at /usr/bin/cargo still wins in PATH. That is deliberate — nothing
# about the desktop Gentoo setup changed — but it means this script must put
# rustup's cargo first explicitly. Do not "simplify" that away.
set -euo pipefail

NDK="${ANDROID_NDK_HOME:-$HOME/Android/android-ndk-r27d}"
# Binaries built against API N run on devices at API >= N, so this is a floor,
# not a target. Kept low deliberately: nothing here calls a modern NDK API, and
# a lower floor means the probe runs on anything worth testing on.
API="${ANDROID_API:-30}"
TARGET=aarch64-linux-android
PROFILE="${PROFILE:-release}"

if [[ ! -d $NDK ]]; then
    echo "error: NDK not found at $NDK" >&2
    echo "       set ANDROID_NDK_HOME, or unpack android-ndk-r27d-linux.zip there" >&2
    exit 1
fi

TC="$NDK/toolchains/llvm/prebuilt/linux-x86_64/bin"
CC_BIN="$TC/${TARGET}${API}-clang"

if [[ ! -x $CC_BIN ]]; then
    echo "error: no clang wrapper for API $API at $CC_BIN" >&2
    echo "       available:" >&2
    ls "$TC" | grep -oE "${TARGET}[0-9]+-clang$" | sed 's/^/         /' >&2
    exit 1
fi

export PATH="$HOME/.cargo/bin:$PATH"
export CARGO_TARGET_AARCH64_LINUX_ANDROID_LINKER="$CC_BIN"
export CC_aarch64_linux_android="$CC_BIN"
export AR_aarch64_linux_android="$TC/llvm-ar"

if ! rustup target list --installed 2>/dev/null | grep -qx "$TARGET"; then
    echo "error: rust std for $TARGET is not installed" >&2
    echo "       run: rustup target add $TARGET" >&2
    exit 1
fi

cd "$(dirname "$0")/../src/hadal-brokerd"

flags=(--target "$TARGET")
[[ $PROFILE == release ]] && flags+=(--release)

echo "building hadal-probe for $TARGET (API $API, $PROFILE)"
cargo build "${flags[@]}" --bin hadal-probe

out="target/$TARGET/$PROFILE/hadal-probe"
echo
file "$out"
ls -lh "$out" | awk '{print "  size: " $5}'
