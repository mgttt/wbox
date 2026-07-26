#!/usr/bin/env bash
# run-guest-tests.sh — build & run the guest C regression suite under wbox-linux.
#
# Style aligned with scripts/test-matrix.sh (PASS/FAIL/SKIP counters, wine|native).
#
# Usage:
#   tests/run-guest-tests.sh [wbox-linux.exe] [--skip-slow]
#
# Env:
#   WBOX_LINUX          wbox-linux.exe path (else $1, else ./vendor/blink/build-win32/wbox-linux.exe)
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
  for c in ./vendor/blink/build-win32/wbox-linux.exe ./wbox-linux.exe; do
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
report() { # report <status> <name> [detail]
  case $1 in
    PASS) pass=$((pass+1)); [ "$LIST" = 1 ] && echo "PASS $2" || printf 'PASS  %s\n' "$2" ;;
    SKIP) skip=$((skip+1)); [ "$LIST" = 1 ] && echo "SKIP $2" || printf 'SKIP  %s%s\n' "$2" "${3:+ —— $3}" ;;
    FAIL) fail=$((fail+1)); [ "$LIST" = 1 ] && echo "FAIL $2" || printf 'FAIL  %s%s\n' "$2" "${3:+ —— $3}" ;;
  esac
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
  fi
done

[ "$LIST" = 1 ] || echo "guest-suite: PASS=$pass FAIL=$fail SKIP=$skip"
[ "$fail" -eq 0 ]
