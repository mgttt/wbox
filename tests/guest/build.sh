#!/bin/sh
# build.sh — compile all guest test sources (t_*.c) into static musl binaries.
#
# Output: tests/guest/bin/<name>  (gitignored; never committed)
#
# Compiler selection (first match wins):
#   $WBOX_ZIG           explicit zig binary
#   zig on PATH
#   python3 -m ziglang  (pip install ziglang)
# Override the whole compiler command via $CC (must accept zig-cc style args).
set -eu

DIR=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
BIN=$DIR/bin
mkdir -p "$BIN"

if [ -n "${CC:-}" ]; then
  ZCC="$CC"
elif [ -n "${WBOX_ZIG:-}" ]; then
  ZCC="$WBOX_ZIG cc"
elif command -v zig >/dev/null 2>&1; then
  ZCC="zig cc"
elif python3 -c 'import ziglang' >/dev/null 2>&1; then
  ZCC="python3 -m ziglang cc"
else
  echo "build.sh: no zig compiler found (install zig or pip install ziglang)" >&2
  exit 1
fi

TARGET=${WBOX_GUEST_TARGET:-x86_64-linux-musl}
CFLAGS=${WBOX_GUEST_CFLAGS:--O2 -Wall}

fail=0
for src in "$DIR"/t_*.c; do
  name=$(basename "$src" .c)
  out=$BIN/$name
  echo "[build] $name"
  # shellcheck disable=SC2086
  if ! $ZCC -target "$TARGET" -static $CFLAGS -I"$DIR" -o "$out" "$src"; then
    echo "build.sh: FAILED to build $name" >&2
    fail=1
  fi
done
exit $fail
