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
build_one() { # build_one <source> <output-name> [extra compiler flags...]
  src=$1
  name=$2
  shift 2
  out=$BIN/$name
  echo "[build] $name"
  # 先编到本地临时盘再拷入 $BIN：部分宿主文件系统（FUSE/网络盘）上
  # zig 直写产物会偶发零填充损坏；cp+校验可检出该环境噪音。
  tmp=$(mktemp "${TMPDIR:-/tmp}/wbox-guest-build.XXXXXX")
  # shellcheck disable=SC2086
  if ! $ZCC -target "$TARGET" -static $CFLAGS -I"$DIR" "$@" -o "$tmp" "$src"; then
    echo "build.sh: FAILED to build $name" >&2
    rm -f "$tmp"
    return 1
  fi
  # 拷贝后校验：cmp 优先；精简环境（如 CI 上只装了必要包的 MSYS2）可能没有
  # diffutils，此时退化为比字节数——比不校验强，且不会把"工具缺失"误报成
  # "拷贝损坏"（真机 CI 上就因此把整套 guest 用例判成构建失败）。
  cpok=0
  if cp "$tmp" "$out"; then
    if command -v cmp >/dev/null 2>&1; then
      cmp -s "$tmp" "$out" && cpok=1
    else
      [ "$(wc -c <"$tmp")" = "$(wc -c <"$out")" ] && cpok=1
    fi
  fi
  if [ "$cpok" -ne 1 ]; then
    echo "build.sh: FAILED to install $name (copy verify mismatch)" >&2
    rm -f "$tmp"
    return 1
  fi
  rm -f "$tmp"
}

for src in "$DIR"/t_*.c; do
  name=$(basename "$src" .c)
  build_one "$src" "$name" || fail=1
done

# Handler delivery has a different owning baseline from signalfd itself. Build a
# second ELF from the same source so C-fixture debt does not grow merely to gain
# file-level baseline precision.
build_one "$DIR/t_signalfd.c" t_signal_handler \
  -DWBOX_SIGNAL_HANDLER_ONLY || fail=1

exit $fail
