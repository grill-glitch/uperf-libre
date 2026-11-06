#!/usr/bin/env bash
# Rebuild magisk/bin/busybox/busybox from upstream source.
#
# The module ships a static aarch64 busybox so its shell scripts get the same toolbox on
# every ROM, instead of trusting whatever toybox the ROM happens to have. This script is
# the whole provenance chain: signed tarball -> our patch -> our config -> the shipped ELF.
#
#   ./build-busybox.sh            # builds into ./out, prints sha256
#   ./build-busybox.sh --install  # additionally copies the result into magisk/bin/busybox/
#
# Requirements: curl, gnupg, bzip2, patch, make, and an NDK (default $HOME/Android/Sdk/ndk/android-ndk-r30).
#
# Expected result for the shipped binary (see README.md for the full table):
#   sha256 6f97e3ea...  because SOURCE_DATE_EPOCH and TZ are pinned below; change either
#   and the banner `BusyBox v1.38.0 (YYYY-MM-DD HH:MM:SS TZ)` changes, and so does the hash.
set -euo pipefail

VERSION=1.38.0
API=26                       # aarch64-linux-android26-; bionic has getrandom/strchrnul by then
TARBALL="busybox-${VERSION}.tar.bz2"
SIG="${TARBALL}.sig"
KEY_FPR=C9E9416F76E610DBD09D040F47B70C55ACC9965B   # Denys Vlasenko, the busybox maintainer
EXPECTED_TARBALL_SHA256=34f9ea6ff8636f2c9241153b9114eefa9e65674a45318ae1ef95bb5f31c53bb2

here=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
NDK=${NDK:-$HOME/Android/Sdk/ndk/android-ndk-r30}
TOOLCHAIN="$NDK/toolchains/llvm/prebuilt/linux-x86_64/bin"
export PATH="$TOOLCHAIN:$PATH"

# Pinned so the embedded build timestamp does not change between runs.
export SOURCE_DATE_EPOCH=1767225600
export TZ=Asia/Shanghai

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
cd "$work"

echo "== fetching busybox ${VERSION} and its signature"
curl -fsSLO "https://busybox.net/downloads/${TARBALL}"
curl -fsSLO "https://busybox.net/downloads/${SIG}"

echo "== verifying the detached signature"
export GNUPGHOME="$work/gnupg"; mkdir -p "$GNUPGHOME"; chmod 700 "$GNUPGHOME"
gpg --batch --quiet --import "$here/denys-vlasenko-pubkey.asc"
fpr=$(gpg --with-colons --fingerprint "$KEY_FPR" | awk -F: '/^fpr/{print $10; exit}')
[ "$fpr" = "$KEY_FPR" ] || { echo "signing key fingerprint is $fpr, expected $KEY_FPR" >&2; exit 1; }
gpg --batch --verify "$SIG" "$TARBALL"
echo "   signed by $KEY_FPR (Denys Vlasenko)"

echo "== checking the tarball digest"
got=$(sha256sum "$TARBALL" | cut -d' ' -f1)
[ "$got" = "$EXPECTED_TARBALL_SHA256" ] || {
    echo "tarball sha256 is $got, expected $EXPECTED_TARBALL_SHA256" >&2; exit 1; }

echo "== unpacking and patching for bionic"
tar xf "$TARBALL"
cd "busybox-${VERSION}"
patch -p1 < "$here/uperf-bionic.patch"
cp "$here/busybox-aarch64-uperf.config" .config

MAKE_VARS=(ARCH=arm64
           CROSS_COMPILE="aarch64-linux-android${API}-"
           CC="aarch64-linux-android${API}-clang"
           AR=llvm-ar NM=llvm-nm RANLIB=llvm-ranlib STRIP=llvm-strip OBJCOPY=llvm-objcopy
           HOSTCC=clang LDFLAGS="-static -no-pie")

echo "== resolving the config against this busybox version"
# `yes ""` feeds the default answer to every new symbol; it dies of SIGPIPE when kconfig
# stops reading, which would trip pipefail, so drop pipefail for this one pipeline.
set +o pipefail
yes "" | make "${MAKE_VARS[@]}" oldconfig >/dev/null
set -o pipefail

echo "== building (static, non-PIE, aarch64)"
make "${MAKE_VARS[@]}" -j"$(nproc)"

file ./busybox
strings -a ./busybox > banner.txt
grep -m1 -oE 'BusyBox v[0-9]+\.[0-9]+\.[0-9]+ \([^)]*\)' banner.txt   # version + the pinned build date
echo -n "sha256:  "; sha256sum ./busybox

if [ "${1:-}" = "--install" ]; then
    install -m 0755 ./busybox "$here/../../../magisk/bin/busybox/busybox"
    echo "installed into magisk/bin/busybox/busybox"
fi
