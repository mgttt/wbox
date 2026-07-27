#!/usr/bin/env bash
# run-guest-tests.sh — build & run the guest C regression suite under wbox-linux.
#
# Style aligned with scripts/test-matrix.sh (PASS/FAIL/SKIP counters, wine|native).
#
# Usage:
#   tests/run-guest-tests.sh [wbox-linux.exe] [--skip-slow]
#
# Env:
#   WBOX_LINUX          wbox-linux.exe path (else $1, else ./target/release/wbox-linux.exe)
#   WBOX_MATRIX_MODE    native|wine|auto (same detection as test-matrix.sh)
#   WINE                wine binary (wine mode)
#   WBOX_GUEST_PREBUILT=1  do NOT compile; use prebuilt tests/guest/bin/*
#                          (CI real-Windows path: binaries uploaded as artifact)
#   WBOX_GUEST_SKIP=1   whole suite reported as SKIP (single line)
#   WBOX_GUEST_LIST=1   machine-readable per-file lines only: "PASS t_foo" /
#                       "FAIL t_foo" / "SKIP t_foo" (used by test-matrix.sh F group)
#   WBOX_GUEST_TIMEOUT  per-test timeout seconds (default 120)
set -u

DIR=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
GUEST=$DIR/guest
BIN=$GUEST/bin

WBOX_LINUX=${WBOX_LINUX:-}
SKIP_SLOW=0
for a in "$@"; do
  case $a in
    --skip-slow) SKIP_SLOW=1 ;;
    -*) ;;
    *) [ -z "$WBOX_LINUX" ] && WBOX_LINUX=$a ;;
  esac
done
if [ -z "$WBOX_LINUX" ]; then
  for c in ./target/release/wbox-linux.exe ./wbox-linux.exe; do
    [ -f "$c" ] && WBOX_LINUX=$c && break
  done
fi
die() { echo "FATAL: $*" >&2; exit 1; }
{ [ -n "$WBOX_LINUX" ] && [ -f "$WBOX_LINUX" ]; } || \
  die "wbox-linux.exe not found (arg1 or WBOX_LINUX)"

MODE=${WBOX_MATRIX_MODE:-auto}
if [ "$MODE" = auto ]; then
  case "${OSTYPE:-}" in
    msys*|cygwin*|win32) MODE=native ;;
    *)                   MODE=wine ;;
  esac
fi
RUN=()
if [ "$MODE" = wine ]; then
  WINE=${WINE:-wine}
  command -v "$WINE" >/dev/null 2>&1 || die "wine mode but wine not found (WINE=$WINE)"
  RUN=(env WINEDEBUG=-all "$WINE")
fi

LIST=${WBOX_GUEST_LIST:-0}
pass=0; fail=0; skip=0
# 基线判定需要按名字比对，故 report 同时记录名字（空格分隔集合）。
passed_names=; failed_names=
report() { # report <status> <name> [detail]
  case $1 in
    PASS) pass=$((pass+1)); passed_names="$passed_names $2"
          [ "$LIST" = 1 ] && echo "PASS $2" || printf 'PASS  %s\n' "$2" ;;
    SKIP) skip=$((skip+1)); [ "$LIST" = 1 ] && echo "SKIP $2" || printf 'SKIP  %s%s\n' "$2" "${3:+ —— $3}" ;;
    FAIL) fail=$((fail+1)); failed_names="$failed_names $2"
          [ "$LIST" = 1 ] && echo "FAIL $2" || printf 'FAIL  %s%s\n' "$2" "${3:+ —— $3}" ;;
  esac
}

# in_set <name> <set...> —— 名字是否在空格分隔集合中
in_set() {
  needle=$1; shift
  for x in $*; do [ "$x" = "$needle" ] && return 0; done
  return 1
}

if [ "${WBOX_GUEST_SKIP:-0}" = 1 ]; then
  report SKIP guest-suite "WBOX_GUEST_SKIP=1"
  echo "guest-suite: PASS=$pass FAIL=$fail SKIP=$skip"
  exit 0
fi

# ---------- build or reuse prebuilt ----------
if [ "${WBOX_GUEST_PREBUILT:-0}" != 1 ]; then
  if ! sh "$GUEST/build.sh" >/dev/null; then
    die "guest build failed (set WBOX_GUEST_PREBUILT=1 to use prebuilt binaries)"
  fi
fi
shopt -s nullglob
built=("$BIN"/t_*)
[ ${#built[@]} -gt 0 ] || die "no guest binaries in $BIN (build.sh produced nothing)"

# ---------- workdir ----------
WORK=$(mktemp -d 2>/dev/null || mktemp -d -t wbox-guest)
trap 'rm -rf "$WORK"' EXIT
for b in "${built[@]}"; do cp "$b" "$WORK/"; done
WBOX_ABS=$(cd "$(dirname "$WBOX_LINUX")" && pwd)/$(basename "$WBOX_LINUX")
cd "$WORK" || die "cannot cd $WORK"

TIMEOUT=${WBOX_GUEST_TIMEOUT:-120}
[ "$LIST" = 1 ] || {
  echo "[mode] $MODE"
  echo "[wbox-linux] $WBOX_ABS"
  echo "[guest-bin] $BIN ($([ "$SKIP_SLOW" = 1 ] && echo 'skip slow' || echo 'all'))"
}

for b in "$WORK"/t_*; do
  name=$(basename "$b")
  rel=./$name
  case $name in
    t_stress*) [ "$SKIP_SLOW" = 1 ] && { report SKIP "$name" "slow (--skip-slow)"; continue; } ;;
  esac
  OUT=$(timeout "$TIMEOUT" "${RUN[@]}" "$WBOX_ABS" "$rel" 2>&1); rc=$?
  OUT=$(printf '%s' "$OUT" | tr -d '\r')
  probe_detail=
  if [ "$name" = t_fork_mem ] && [ "$rc" -eq 0 ]; then
    for stage in system machine args thread; do
      PROBE=$(env WBOX_TEST_FORK_FAIL="$stage" \
        timeout "$TIMEOUT" "${RUN[@]}" "$WBOX_ABS" "$rel" \
        --fork-failure 2>&1)
      prc=$?
      if [ "$prc" -ne 0 ]; then
        OUT="$OUT
FAIL fork/failure-$stage: rc=$prc $(printf '%s' "$PROBE" | tail -2 | head -c 160)"
        rc=$prc
        break
      fi
      probe_detail="${probe_detail}${probe_detail:+,}$stage"
    done
  fi
  if [ "$name" = t_mmap ] && [ "$rc" -eq 0 ]; then
    for stage in msync munmap munmap-second split fixed; do
      fault=writeback
      [ "$stage" = munmap-second ] && fault=writeback-second
      [ "$stage" = split ] && fault=split
      PROBE=$(env WBOX_TEST_FSHARE_FAIL="$fault" \
        timeout "$TIMEOUT" "${RUN[@]}" "$WBOX_ABS" "$rel" \
        "--writeback-failure-$stage" 2>&1)
      prc=$?
      if [ "$prc" -ne 0 ]; then
        OUT="$OUT
FAIL mmap/writeback-failure-$stage: rc=$prc $(printf '%s' "$PROBE" | tail -2 | head -c 160)"
        rc=$prc
        break
      fi
      probe_detail="${probe_detail}${probe_detail:+,}$stage"
    done
  fi
  nfail=$(printf '%s\n' "$OUT" | grep -c '^FAIL ' || true)
  if [ "$rc" -eq 124 ]; then
    report FAIL "$name" "timeout ${TIMEOUT}s"
  elif [ "$rc" -ne 0 ]; then
    detail=$(printf '%s\n' "$OUT" | grep '^FAIL ' | head -2 | tr '\n' ' ')
    [ -n "$detail" ] || detail=$(printf '%s' "$OUT" | tail -2 | head -c 200)
    report FAIL "$name" "rc=$rc $detail"
    [ "$LIST" = 1 ] || printf '%s\n' "$OUT" | sed 's/^/    /'
  elif [ "$nfail" -gt 0 ]; then
    report FAIL "$name" "$(printf '%s\n' "$OUT" | grep '^FAIL ' | head -2 | tr '\n' ' ')"
    [ "$LIST" = 1 ] || printf '%s\n' "$OUT" | grep '^FAIL ' | sed 's/^/    /'
  else
    sum=$(printf '%s\n' "$OUT" | grep '^SUMMARY ' | tail -1)
    report PASS "$name" "$sum"
    [ "$LIST" = 1 ] || printf '    %s\n' "$sum"
    if [ "$LIST" != 1 ] && [ -n "$probe_detail" ]; then
      case $name in
        t_fork_mem) printf '    fork failure probes: %s\n' "$probe_detail" ;;
        t_mmap) printf '    fshare failure probes: %s\n' "$probe_detail" ;;
      esac
    fi
  fi
done

[ "$LIST" = 1 ] || echo "guest-suite: PASS=$pass FAIL=$fail SKIP=$skip"

# ---- 已知失败基线判定 ----
# 与机器基线比对，使暂存的已知失败可被精确登记，同时要求修复后立即收紧：
#   失败 ⊆ 基线                 → 0（已知失败，放行）
#   出现基线外的新失败           → 1（回归）
#   基线内的用例变成通过         → 1（基线过期，必须同步收紧；见 docs/testing.md §四）
BASELINE=${WBOX_GUEST_BASELINE:-$DIR/known-failures.txt}
if [ "${WBOX_GUEST_NO_BASELINE:-0}" = 1 ] || [ ! -f "$BASELINE" ]; then
  [ "$LIST" = 1 ] || {
    [ -f "$BASELINE" ] || echo "guest-suite: 未找到基线文件 $BASELINE —— 按'任一失败即非零'判定"
  }
  [ "$fail" -eq 0 ]
  exit $?
fi

# 基线条目支持可选的模式标注：`<用例名> [@native|@wine]`。
# 起因（实测）：t_path 的 path/long-nested 只在**真 Windows** 上失败——
# 宿主绝对路径超过 MAX_PATH(260)，而 wine 无此限制故通过。没有模式标注时，
# 把它写进基线会让 wine 下报"基线过期"，不写又会让真机报"回归"，
# 两种环境无法共用一份基线。带标注的条目只在对应模式下生效。
known=$(sed -e 's/#.*//' -e '/^[[:space:]]*$/d' "$BASELINE" | tr -d '\r' |
  while read -r name tag _rest; do
    [ -n "$name" ] || continue
    case $tag in
      "")        printf '%s ' "$name" ;;
      @native)   [ "$MODE" = native ] && printf '%s ' "$name" ;;
      @wine)     [ "$MODE" = wine ]   && printf '%s ' "$name" ;;
      *) echo "guest-suite: 基线条目 '$name' 的标注 '$tag' 无法识别（仅支持 @native/@wine）" >&2 ;;
    esac
  done)
regressions=; fixed=
for n in $failed_names; do
  in_set "$n" "$known" || regressions="$regressions $n"
done
for n in $known; do
  in_set "$n" "$passed_names" && fixed="$fixed $n"
done

rc=0
if [ -n "$regressions" ]; then
  echo "guest-suite: ✗ 基线外的新失败（回归）：$regressions" >&2
  rc=1
fi
if [ -n "$fixed" ]; then
  echo "guest-suite: ✗ 基线内用例已通过，基线过期：$fixed" >&2
  echo "guest-suite:   请从 $BASELINE 移除这些条目，并同步 KNOWN-FAILURES.md" >&2
  rc=1
fi
if [ "$rc" = 0 ] && [ -n "$failed_names" ]; then
  echo "guest-suite: ✓ 失败项均在已知基线内（$failed_names）"
fi
exit $rc
