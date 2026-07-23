#!/bin/sh
# wbox win32 L1 build script: cross-compiles blink -> wbox-linux.exe
# with a MinGW-w64 clang/gcc toolchain.
#
# Parameterized via environment:
#   WBOX_CC      compiler driver. Either a real compiler
#                (e.g. x86_64-w64-mingw32-gcc, clang --target=...) or
#                "zig" to use `python3 -m ziglang cc` (default fallback).
#   WBOX_TARGET  target triple        (default x86_64-windows-gnu)
#   WBOX_BUILD   build/output dir     (default <repo>/build-win32)
#   WBOX_JOBS    parallel jobs        (default nproc)
#
# Example (llvm-mingw):
#   WBOX_CC=/tmp/wbox5/llvm-mingw/bin/x86_64-w64-mingw32-clang sh build-mingw.sh
set -e
cd "$(dirname "$0")/.."          # -> vendor/blink
BLINK_DIR=$PWD
BUILD=${WBOX_BUILD:-$BLINK_DIR/build-win32}
TARGET=${WBOX_TARGET:-x86_64-windows-gnu}
JOBS=${WBOX_JOBS:-$(nproc 2>/dev/null || echo 4)}

if [ -n "$WBOX_CC" ]; then
  CC=$WBOX_CC
elif command -v x86_64-w64-mingw32-gcc >/dev/null 2>&1; then
  CC=x86_64-w64-mingw32-gcc
else
  CC="python3 -m ziglang cc -target $TARGET"
fi
echo "CC = $CC"

CFLAGS="-O2 -g0 -fno-ident -DNDEBUG -Wno-everything \
  -Iwin32/compat -Iwin32 -I. -Ithird_party/libz -include win32/config.h"
# zlib is plain C and must NOT see our POSIX compat headers (io.h clashes).
ZCFLAGS="-O2 -g0 -fno-ident -DNDEBUG -Wno-everything -I. -Ithird_party/libz"

# All blink core sources except the blinkenlights TUI frontend and the
# oneoff test utility (both carry their own main()).
SRCS=$(ls blink/*.c | grep -v -e 'blink/blinkenlights.c' -e 'blink/oneoff.c' \
        -e 'blink/jit.c' -e 'blink/jitflush.c')
SRCS="$SRCS win32/w32mem.c win32/w32fd.c win32/w32proc.c win32/w32sig.c win32/w32stubs.c"
# zlib core only: gz*.c pull in <io.h> and blink only needs
# compress2/uncompress/compressBound.
ZSRCS=$(ls third_party/libz/*.c | grep -v '/gz')

mkdir -p "$BUILD/obj"
OBJLIST=$BUILD/objlist.txt
: >"$OBJLIST"

compile_one() {
  src=$1
  obj="$BUILD/obj/$(echo "$src" | tr '/.' '__').o"
  echo "$obj" >>"$OBJLIST"
  case $src in
    third_party/libz/*) $CC $ZCFLAGS -c "$src" -o "$obj" ;;
    *) $CC $CFLAGS -c "$src" -o "$obj" ;;
  esac
}
export CC CFLAGS ZCFLAGS BUILD OBJLIST

# poor man's parallel build
i=0
for src in $SRCS $ZSRCS; do
  compile_one "$src" &
  i=$((i + 1))
  if [ $((i % JOBS)) -eq 0 ]; then wait; fi
done
wait

echo "linking..."
$CC -O2 -o "$BUILD/wbox-linux.exe" @"$OBJLIST" -lws2_32 -lwinmm -lbcrypt
ls -la "$BUILD/wbox-linux.exe"
echo "built: $BUILD/wbox-linux.exe"
