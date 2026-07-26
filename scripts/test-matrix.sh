#!/usr/bin/env bash
# test-matrix.sh —— wbox-linux 验收矩阵产品化脚本
#
# 矩阵内容（与 vendor/blink/WIN32-PORT.md §7/§7.3/§7.4 的 wine 实测口径一致）：
#   A. 基础矩阵 11 项（uname/echo/cat/ls/stat/find/重定向/退出码…）
#   B. shell 矩阵 8 项（管道/命令替换/后台+wait/重定向链/dev/null/fork 子 exec）
#   C. fork 矩阵（子 shell 顺序/嵌套命令替换/后台任务产出）
#   D. 网络：busybox wget 公网下载 + md5 精确比对（回归保护值）
#   E. epoll loopback 单测（需预编译静态二进制，缺失则 SKIP 并提示）
#
# 运行模式（按 OSTYPE 自动检测，也可用 WBOX_MATRIX_MODE=native|wine 强制）：
#   native —— msys2 / Git Bash / Cygwin：直接执行 wbox-linux.exe（真 Windows）
#   wine   —— Linux + wine：以 `wine wbox-linux.exe ...` 前缀执行
#
# 输入（位置参数优先，其次环境变量）：
#   $1 / WBOX_LINUX     wbox-linux.exe 路径
#                       （默认 ./vendor/blink/build-win32/wbox-linux.exe 或 ./wbox-linux.exe）
#   $2 / BUSYBOX        Linux x86_64 **静态** busybox 路径（默认 ./busybox）
#   WINE                wine 二进制（wine 模式默认 `wine`）
#   EPOLL_TEST_BIN      epoll loopback 静态单测二进制（可选；缺失则 SKIP）
#   WGET_URL            下载 URL（默认 http://mirrors.aliyun.com/debian/README）
#   WGET_MD5            期望 md5（默认 01718454f79b3bd9fa51e0e1f8966103，1193 字节版本）
#   WBOX_MATRIX_NET_SKIP=1  网络用例失败时记 SKIP 而非 FAIL（CI 真机网络不可达保护）
#
# 退出码：0 = 全部通过（允许 SKIP）；1 = 存在 FAIL 或参数错误。
set -u

# ---------- 参数与环境 ----------
WBOX_LINUX=${1:-${WBOX_LINUX:-}}
BUSYBOX=${2:-${BUSYBOX:-}}
if [ -z "$WBOX_LINUX" ]; then
  for c in ./vendor/blink/build-win32/wbox-linux.exe ./wbox-linux.exe; do
    [ -f "$c" ] && WBOX_LINUX=$c && break
  done
fi
BUSYBOX=${BUSYBOX:-./busybox}
WGET_URL=${WGET_URL:-http://mirrors.aliyun.com/debian/README}
WGET_MD5=${WGET_MD5:-01718454f79b3bd9fa51e0e1f8966103}
NET_SKIP=${WBOX_MATRIX_NET_SKIP:-0}

fail=0; pass=0; skip=0

die() { echo "FATAL: $*" >&2; exit 1; }

{ [ -n "$WBOX_LINUX" ] && [ -f "$WBOX_LINUX" ]; } || die "找不到 wbox-linux.exe（用 \$1 或 WBOX_LINUX 指定）"
[ -f "$BUSYBOX" ] || die "找不到 busybox（用 \$2 或 BUSYBOX 指定；需 Linux x86_64 静态版，如 https://busybox.net/downloads/binaries/1.35.0-x86_64-linux-musl/busybox）"

# ---------- 模式检测 ----------
MODE=${WBOX_MATRIX_MODE:-auto}
if [ "$MODE" = auto ]; then
  case "${OSTYPE:-}" in
    msys*|cygwin*|win32) MODE=native ;;
    *)                   MODE=wine ;;
  esac
fi

RUN=()  # 执行前缀：wine 模式为 [env WINEDEBUG=-all wine]，native 为空
if [ "$MODE" = wine ]; then
  WINE=${WINE:-wine}
  command -v "$WINE" >/dev/null 2>&1 || die "wine 模式但找不到 wine（WINE=$WINE）"
  RUN=(env WINEDEBUG=-all "$WINE")
  echo "[mode] wine ($WINE)"
else
  echo "[mode] native (OSTYPE=${OSTYPE:-unknown})"
fi
echo "[wbox-linux] $WBOX_LINUX"
echo "[busybox]    $BUSYBOX"

# ---------- 工作目录 ----------
# busybox 等以**相对路径**喂给 wbox-linux：wine 与真 Windows 对 argv 中
# 的 unix 绝对路径处理不同，相对路径在两种模式下都安全。
WORK=$(mktemp -d 2>/dev/null || mktemp -d -t wbox-matrix)
trap 'rm -rf "$WORK"' EXIT
cp "$BUSYBOX" "$WORK/busybox" || die "无法复制 busybox 到 $WORK"
if [ -n "${EPOLL_TEST_BIN:-}" ] && [ -f "${EPOLL_TEST_BIN:-/nonexistent}" ]; then
  cp "$EPOLL_TEST_BIN" "$WORK/epoll-loopback"
  EPOLL_LOCAL=epoll-loopback
else
  EPOLL_LOCAL=
fi

# 基础矩阵所需夹具
printf 'line1 from host file\n' > "$WORK/t.txt"
mkdir -p "$WORK/subdir"
printf 'a\n' > "$WORK/subdir/a.txt"
printf 'b\n' > "$WORK/subdir/b.txt"

WBOX_ABS=$(cd "$(dirname "$WBOX_LINUX")" && pwd)/$(basename "$WBOX_LINUX")
cd "$WORK" || die "无法进入工作目录"

# bb <args...> —— 跑 busybox 小程序，输出捕获到 $OUT，返回 rc
bb() {
  OUT=$("${RUN[@]}" "$WBOX_ABS" busybox "$@" 2>&1)
  rc=$?
  # 剥 \r：真 Windows 下控制台/管道输出可能带 CRLF
  OUT=$(printf '%s' "$OUT" | tr -d '\r')
  return $rc
}

report() { # report <status> <name> [detail]
  case $1 in
    PASS) pass=$((pass+1)); printf 'PASS  %s\n' "$2" ;;
    SKIP) skip=$((skip+1)); printf 'SKIP  %s%s\n' "$2" "${3:+ —— $3}" ;;
    FAIL) fail=$((fail+1)); printf 'FAIL  %s%s\n' "$2" "${3:+ —— $3}" ;;
  esac
}

# check_rc_out <name> <expected-rc> <expected-substr-or-empty> <cmd...>
check_rc_out() {
  name=$1; want_rc=$2; want_out=$3; shift 3
  bb "$@"; rc=$?
  if [ "$rc" -ne "$want_rc" ]; then
    report FAIL "$name" "rc=$rc（期望 $want_rc）输出: $(printf '%s' "$OUT" | head -c 200)"
    return
  fi
  if [ -n "$want_out" ] && ! printf '%s' "$OUT" | grep -qF "$want_out"; then
    report FAIL "$name" "输出缺少 '$want_out'：$(printf '%s' "$OUT" | head -c 200)"
    return
  fi
  report PASS "$name"
}

echo
echo "=== A. 基础矩阵（11 项）==="
check_rc_out "A1  uname -a"            0 Linux        uname -a
check_rc_out "A2  echo hello wbox"     0 "hello wbox" echo hello wbox
check_rc_out "A3  cat t.txt"           0 "line1 from host file" cat t.txt
check_rc_out "A4  ls -la t.txt"        0 t.txt        ls -la t.txt
check_rc_out "A5  ls -la subdir"       0 a.txt        ls -la subdir
check_rc_out "A6  ls 列目录"           0 subdir       ls
check_rc_out "A7  sh -c echo hi"       0 hi           sh -c 'echo hi'
# A8 退出码转发：true/false/exit 7
bb true; rc_true=$?
bb false; rc_false=$?
bb sh -c 'exit 7'; rc_7=$?
if [ "$rc_true" -eq 0 ] && [ "$rc_false" -eq 1 ] && [ "$rc_7" -eq 7 ]; then
  report PASS "A8  退出码转发 (0/1/7)"
else
  report FAIL "A8  退出码转发" "true=$rc_true false=$rc_false exit7=$rc_7（期望 0/1/7）"
fi
check_rc_out "A9  stat t.txt"          0 "Size:"      stat t.txt
check_rc_out "A10 find subdir"         0 subdir/a.txt find subdir
# A11 重定向写 + 读回
bb sh -c 'echo abc > out3.txt'; rc_w=$?
bb cat out3.txt; rc_r=$?
if [ "$rc_w" -eq 0 ] && [ "$rc_r" -eq 0 ] && [ "$OUT" = abc ]; then
  report PASS "A11 重定向写读回"
else
  report FAIL "A11 重定向写读回" "写 rc=$rc_w 读 rc=$rc_r 内容='$OUT'"
fi

echo
echo "=== B. shell 矩阵（8 项，fork 依赖）==="
check_rc_out "B1  管道 echo hello | cat" 0 hello  sh -c 'echo hello | cat'
check_rc_out "B2  命令替换"              0 inner  sh -c 'echo $(echo inner)'
check_rc_out "B3  后台任务 sleep & wait" 0 ""     sh -c 'sleep 0.1 & wait'
check_rc_out "B4  重定向链 && cat"       0 x      sh -c 'echo x > f && cat f'
check_rc_out "B5  写 /dev/null"          0 ""     sh -c 'echo n > /dev/null'
check_rc_out "B6  读 /dev/null"          0 ok     sh -c 'cat /dev/null; echo ok'
check_rc_out "B7  管道 + grep 退出码"    0 ""     sh -c 'echo a | grep a'
# B8 fork 子 exec 外部 applet（回归保护项，md5("hello")）
check_rc_out "B8  fork 子 exec md5sum"   0 5d41402abc4b2a76b9719d911017c592 \
  sh -c 'echo -n hello | md5sum'

echo
echo "=== C. fork 矩阵 ==="
check_rc_out "C1  子 shell 顺序执行"   0 3       sh -c 'for i in 1 2 3; do (echo $i); done'
check_rc_out "C2  嵌套命令替换"        0 deep    sh -c 'echo $(echo $(echo deep))'
check_rc_out "C3  后台任务完成后产出"  0 bg-done sh -c 'sleep 0.1 & wait; echo bg-done'

echo
echo "=== D. 网络矩阵：wget + md5 校验 ==="
bb wget -q -O readme.dl "$WGET_URL"; rc_dl=$?
if [ "$rc_dl" -eq 0 ]; then
  bb md5sum readme.dl; rc_md=$?
  got_md5=$(printf '%s' "$OUT" | awk '{print $1}')
  if [ "$rc_md" -eq 0 ] && [ "$got_md5" = "$WGET_MD5" ]; then
    report PASS "D1  wget $WGET_URL md5=$got_md5"
  elif [ "$NET_SKIP" = 1 ]; then
    report SKIP "D1  wget md5 校验" "md5 不符（got=$got_md5 want=$WGET_MD5），NET_SKIP=1"
  else
    report FAIL "D1  wget md5 校验" "md5 不符 got=$got_md5 want=$WGET_MD5"
  fi
else
  if [ "$NET_SKIP" = 1 ]; then
    report SKIP "D1  wget $WGET_URL" "下载失败 rc=$rc_dl（网络不可达），NET_SKIP=1"
  else
    report FAIL "D1  wget $WGET_URL" "下载失败 rc=$rc_dl：$(printf '%s' "$OUT" | head -c 200)"
  fi
fi

echo
echo "=== E. epoll loopback 单测 ==="
if [ -n "$EPOLL_LOCAL" ]; then
  OUT=$("${RUN[@]}" "$WBOX_ABS" "$EPOLL_LOCAL" 2>&1); rc=$?
  OUT=$(printf '%s' "$OUT" | tr -d '\r')
  if [ "$rc" -eq 0 ] && printf '%s' "$OUT" | grep -q EPOLL_LOOPBACK_OK; then
    report PASS "E1  epoll loopback（EPOLL_LOOPBACK_OK）"
  else
    report FAIL "E1  epoll loopback" "rc=$rc：$(printf '%s' "$OUT" | head -c 200)"
  fi
else
  report SKIP "E1  epoll loopback" \
    "无预编译静态二进制（设 EPOLL_TEST_BIN；zig cc -static 自编译，见 WIN32-PORT.md §7.3）"
fi

echo
echo "=== F. guest C 回归套件（tests/run-guest-tests.sh）==="
# 每个 t_* 测试二进制映射为一条 F 项。zig 可用则现场编译；
# CI 真 Windows 无 zig 时设 WBOX_GUEST_PREBUILT=1 使用预编译产物（artifact 上传）。
# WBOX_GUEST_SKIP=1 整组 SKIP。
RUNNER=tests/run-guest-tests.sh
if [ ! -f "$RUNNER" ] && [ -f "$OLDPWD/tests/run-guest-tests.sh" ]; then
  RUNNER=$OLDPWD/tests/run-guest-tests.sh
fi
if [ "${WBOX_GUEST_SKIP:-0}" = 1 ]; then
  report SKIP "F1  guest 套件" "WBOX_GUEST_SKIP=1"
elif [ ! -f "$RUNNER" ]; then
  report SKIP "F1  guest 套件" "tests/run-guest-tests.sh 不存在"
else
  guest_out=$(cd "$OLDPWD" 2>/dev/null && \
    WBOX_LINUX="$WBOX_ABS" WBOX_MATRIX_MODE="$MODE" WINE="${WINE:-wine}" \
    WBOX_GUEST_LIST=1 WBOX_GUEST_PREBUILT="${WBOX_GUEST_PREBUILT:-0}" \
    bash "$RUNNER" 2>&1)
  guest_rc=$?
  # LIST 模式逐行 "PASS t_foo" / "FAIL t_foo" / "SKIP t_foo"
  fi=0
  while IFS= read -r line; do
    case $line in
      PASS\ t_*|FAIL\ t_*|SKIP\ t_*)
        fi=$((fi+1))
        st=${line%% *}; nm=${line#* }
        report "$st" "F$fi  $nm" ;;
    esac
  done <<EOF
$guest_out
EOF
  if [ "$fi" -eq 0 ]; then
    if [ "$guest_rc" -eq 0 ]; then
      report SKIP "F1  guest 套件" "runner 无输出（无 t_* 用例？）"
    else
      report FAIL "F1  guest 套件" "runner 失败 rc=$guest_rc：$(printf '%s' "$guest_out" | tail -2 | head -c 200)"
    fi
  fi
fi

echo
echo "==================================="
echo "结果: PASS=$pass FAIL=$fail SKIP=$skip"
[ "$fail" -eq 0 ] || exit 1
exit 0
