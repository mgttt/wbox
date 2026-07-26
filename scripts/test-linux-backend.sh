#!/usr/bin/env bash
# test-linux-backend.sh —— LinuxNativeBackend（多宿主 §10）的端到端验收。
#
# 覆盖 docs-architecture.md §10.5 的 L1/L2 验收标准，走**完整 CLI 链路**
# （假镜像缓存 → wbox run），而不是只测内部函数：
#   L1  uid 映射 / 新根隔离 / PID namespace / 退出码转发
#   L2  --memory 超限失败 / --max-procs 挡 fork 炸弹 / --cpu-pct 语义
#
# 用法：
#   scripts/test-linux-backend.sh [wbox 二进制] [静态 busybox]
# 默认：target/debug/wbox 与 ./busybox
#
# 约定（与 scripts/test-matrix.sh 一致）：PASS/FAIL/SKIP 计数，退出码即结果。
# 环境能力缺失（无 user namespace 等）记 SKIP 而非 FAIL——那不是代码回归。
set -u

WBOX=${1:-target/debug/wbox}
BUSYBOX=${2:-./busybox}
pass=0; fail=0; skip=0
report() {
  case $1 in
    PASS) pass=$((pass+1)); printf 'PASS  %s\n' "$2" ;;
    SKIP) skip=$((skip+1)); printf 'SKIP  %s%s\n' "$2" "${3:+ —— $3}" ;;
    FAIL) fail=$((fail+1)); printf 'FAIL  %s%s\n' "$2" "${3:+ —— $3}" ;;
  esac
}
die() { echo "FATAL: $*" >&2; exit 1; }

[ -x "$WBOX" ] || die "找不到可执行的 wbox：$WBOX（先 cargo build）"
[ -f "$BUSYBOX" ] || { report SKIP "全部用例" "缺静态 busybox（$BUSYBOX）"; echo "结果: PASS=0 FAIL=0 SKIP=1"; exit 0; }

# 宿主能力探测：user namespace 是 L1 的硬前置。
if ! unshare -Ur --mount true 2>/dev/null; then
  report SKIP "全部用例" "宿主不允许 unprivileged user namespace"
  echo "结果: PASS=$pass FAIL=$fail SKIP=$skip"
  exit 0
fi

WBOX_ABS=$(cd "$(dirname "$WBOX")" && pwd)/$(basename "$WBOX")
BB_ABS=$(cd "$(dirname "$BUSYBOX")" && pwd)/$(basename "$BUSYBOX")

# ---- 造一个"已 pull 的镜像"缓存，让 CLI 走真实的镜像分派路径 ----
WORK=$(mktemp -d "${TMPDIR:-/tmp}/wbox-lbe.XXXXXX") || die "mktemp 失败"
trap 'rm -rf "$WORK"' EXIT
CACHE=$WORK/home/.wbox/images/registry-1.docker.io/library_lbetest/latest
mkdir -p "$CACHE/rootfs/bin" "$CACHE/rootfs/proc" "$CACHE/rootfs/etc"
cp "$BB_ABS" "$CACHE/rootfs/bin/busybox"
chmod +x "$CACHE/rootfs/bin/busybox"
# busybox 按 argv[0] 分派 applet，故给用到的都建符号链接
for a in sh id ls dd sleep echo cat; do
  ln -sf busybox "$CACHE/rootfs/bin/$a"
done
printf '{}\n' > "$CACHE/manifest.json"
printf '["sha256:l1"]\n' > "$CACHE/layers.json"
printf '{"config":{"Env":["PATH=/bin"],"Cmd":["/bin/id"]}}\n' > "$CACHE/config.json"

# run <额外 wbox 参数...> -- <guest 命令...>：输出到 $OUT，退出码到 $rc
run() {
  OUT=$(HOME=$WORK/home "$WBOX_ABS" run "$@" 2>&1)
  rc=$?
  return $rc
}

echo "=== L1 rootless 隔离 ==="

run lbetest -- /bin/id
if printf '%s' "$OUT" | grep -q 'uid=0'; then
  report PASS "L1.1 uid 映射（容器内为 root）"
else
  report FAIL "L1.1 uid 映射" "rc=$rc 输出: $(printf '%s' "$OUT" | head -c 200)"
fi

run lbetest -- /bin/ls /
# 新根应只见 rootfs 内容；宿主特征目录（usr/home/root）不应出现
if printf '%s' "$OUT" | grep -q '^bin$' && ! printf '%s' "$OUT" | grep -qE '^(usr|home|root)$'; then
  report PASS "L1.2 新根隔离（宿主文件系统不可见）"
else
  report FAIL "L1.2 新根隔离" "输出: $(printf '%s' "$OUT" | tr '\n' ' ' | head -c 200)"
fi

run lbetest -- /bin/sh -c 'echo pid=$$'
if printf '%s' "$OUT" | grep -q 'pid=1'; then
  report PASS "L1.3 PID namespace（guest 为 PID 1）"
else
  report FAIL "L1.3 PID namespace" "输出: $(printf '%s' "$OUT" | head -c 200)"
fi

run lbetest -- /bin/sh -c 'exit 7'
if [ "$rc" -eq 7 ]; then
  report PASS "L1.4 退出码转发（exit 7）"
else
  report FAIL "L1.4 退出码转发" "rc=$rc（期望 7）"
fi

echo
echo "=== L2 资源限额 ==="

# 基线：不加限额时同样的分配必须成功，否则下一条断言没有意义
run lbetest -- /bin/sh -c 'dd if=/dev/zero of=/dev/null bs=64M count=1'
base_ok=$rc
run --memory 16 lbetest -- /bin/sh -c 'dd if=/dev/zero of=/dev/null bs=64M count=1'
if [ "$base_ok" -eq 0 ] && [ "$rc" -ne 0 ]; then
  report PASS "L2.1 --memory 16 阻止 64MB 分配（无限额时同操作成功）"
elif [ "$base_ok" -ne 0 ]; then
  report SKIP "L2.1 --memory" "无限额时基线操作本身就失败（rc=$base_ok），无法判定"
else
  report FAIL "L2.1 --memory" "限额下仍成功（rc=$rc）"
fi

# --max-procs 有两种正确结局，取决于宿主能力：
#   有 cgroup v2            → pids.max 生效，fork 炸弹被挡；
#   无 cgroup v2 且非 root  → RLIMIT_NPROC 生效，同样被挡；
#   无 cgroup v2 且是 root  → RLIMIT_NPROC 对特权进程无效，故必须**明确拒绝**
#                             （实测 root 下 40 个子进程全能起来，限不住）。
run --max-procs 8 lbetest -- /bin/sh -c 'i=0; while [ $i -lt 40 ]; do /bin/sleep 5 & i=$((i+1)); done; echo all-spawned'
if [ ! -f /sys/fs/cgroup/cgroup.controllers ] && [ "$(id -u)" = 0 ]; then
  if [ "$rc" -eq 1 ] && printf '%s' "$OUT" | grep -q 'max-procs'; then
    report PASS "L2.2 --max-procs 以 root 且无 cgroup v2 时明确拒绝（RLIMIT_NPROC 限不住 root）"
  else
    report FAIL "L2.2 --max-procs 拒绝语义" "rc=$rc（期望 1）输出: $(printf '%s' "$OUT" | head -c 200)"
  fi
elif printf '%s' "$OUT" | grep -qi "can't fork\|Resource temporarily unavailable"; then
  report PASS "L2.2 --max-procs 8 挡住 fork 炸弹"
else
  report FAIL "L2.2 --max-procs" "未见 fork 失败：$(printf '%s' "$OUT" | head -c 200)"
fi

# --cpu-pct：有 cgroup v2 应正常执行；没有则必须**明确报错**而非静默忽略
run --cpu-pct 50 lbetest -- /bin/id
if [ -f /sys/fs/cgroup/cgroup.controllers ]; then
  if [ "$rc" -eq 0 ] && printf '%s' "$OUT" | grep -q 'uid=0'; then
    report PASS "L2.3 --cpu-pct（cgroup v2 路径生效）"
  else
    report FAIL "L2.3 --cpu-pct（cgroup v2）" "rc=$rc 输出: $(printf '%s' "$OUT" | head -c 200)"
  fi
else
  if [ "$rc" -eq 1 ] && printf '%s' "$OUT" | grep -q 'cpu-pct'; then
    report PASS "L2.3 --cpu-pct 无 cgroup v2 时明确拒绝（rc=1，不静默忽略）"
  else
    report FAIL "L2.3 --cpu-pct 拒绝语义" "rc=$rc（期望 1）输出: $(printf '%s' "$OUT" | head -c 200)"
  fi
fi

echo
echo "==================================="
echo "结果: PASS=$pass FAIL=$fail SKIP=$skip"
[ "$fail" -eq 0 ]
