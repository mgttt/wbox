#!/usr/bin/env bash
# wbox x86_64-pc-windows-gnu linker shim for cargo/rustc.
#
# rustc 向 linker 透传 -nodefaultlibs；zig cc 把它原样交给 zig lld，
# lld 的 no_fallback 模式随后找不到 libmsvcrt.a / libwindows.*.a 等
# 默认库（zig 的 mingw 导入库是按需生成的，不在 lld 默认搜索路径）。
# 解法：用 bash 数组重建参数列表，彻底过滤 -nodefaultlibs / -nostdlib，
# 其余参数原样透传给 zig cc。
set -e
args=()
for a in "$@"; do
  case "$a" in
    -nodefaultlibs|-nostdlib|-nostartfiles) ;;
    *) args+=("$a") ;;
  esac
done
exec python3 -m ziglang cc -target x86_64-windows-gnu "${args[@]}"
