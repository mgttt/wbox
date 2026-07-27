#!/usr/bin/env bash
# test-linux-backend.sh —— LinuxNativeBackend（多宿主 §10）的端到端验收。
#
# 覆盖 PRD.md F5 的 Linux 后端验收标准，走**完整 CLI 链路**
# （假镜像缓存 → wbox run），而不是只测内部函数：
#   L1  uid 映射 / 新根隔离 / PID namespace / 退出码转发
#   L2  --memory 超限失败 / --max-procs 挡 fork 炸弹 / --cpu-pct 语义
#   H   台阶①宿主程序模式（`wbox run -- <本机程序>`，harness 环境控制用）
#   N   网络默认断开（与 Windows 侧默认无 INTERNET_CLIENT 对齐）
#   L3  进程树收割（SIGKILL wbox 后无残留，对齐 KILL_ON_JOB_CLOSE）
#   W   台阶③经 wine 跑 Windows 程序（需 wine + mingw，缺则 SKIP）
#   C   cgroup v2 首选路径（需 WBOX_LBE_CGROUP=1 + 已委派子树，缺则 SKIP）
#
# 用法：
#   scripts/test-linux-backend.sh [wbox 二进制] [静态 busybox]
# 默认：target/debug/wbox 与 ./busybox
#
# 约定（与 scripts/test-matrix.sh 一致）：PASS/FAIL/SKIP 计数，退出码即结果。
# 环境能力缺失（无 user namespace 等）记 SKIP 而非 FAIL——那不是代码回归。
#
# **但 CI 里必须设 `WBOX_LBE_REQUIRE=1`**：那时能力缺失记 FAIL。
# 教训——这个门禁曾经"全绿"却一个用例都没跑：GitHub 的 ubuntu runner 从
# Ubuntu 24.04 起默认由 AppArmor 关掉了 unprivileged user namespace
# (`kernel.apparmor_restrict_unprivileged_userns=1`)，脚本老实地 SKIP 了全部
# 用例并返回 0，于是门禁变成装饰。SKIP 语义本身是对的，错在没人区分
# "本地机器恰好不支持"与"专门为这条门禁准备的 runner 竟然不支持"。
set -u

REQUIRE=${WBOX_LBE_REQUIRE:-0}
# 能力缺失时的记法：CI 里是 FAIL，本地是 SKIP
absent() { if [ "$REQUIRE" = 1 ]; then report FAIL "$1" "$2（WBOX_LBE_REQUIRE=1：本环境本应具备该能力）"; else report SKIP "$1" "$2"; fi; }

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
if [ ! -f "$BUSYBOX" ]; then
  absent "全部用例" "缺静态 busybox（$BUSYBOX）"
  echo "结果: PASS=$pass FAIL=$fail SKIP=$skip"
  [ "$fail" -eq 0 ]
  exit $?
fi

# 宿主能力探测：user namespace 是全部用例的硬前置。
if ! unshare -Ur --mount true 2>/dev/null; then
  hint="宿主不允许 unprivileged user namespace"
  # Ubuntu 24.04+ 的 AppArmor 开关是最常见的原因，直接把解法说出来
  if [ "$(cat /proc/sys/kernel/apparmor_restrict_unprivileged_userns 2>/dev/null)" = 1 ]; then
    hint="$hint（kernel.apparmor_restrict_unprivileged_userns=1，可 sysctl 置 0 打开）"
  fi
  absent "全部用例" "$hint"
  echo "结果: PASS=$pass FAIL=$fail SKIP=$skip"
  [ "$fail" -eq 0 ]
  exit $?
fi

WBOX_ABS=$(cd "$(dirname "$WBOX")" && pwd)/$(basename "$WBOX")
BB_ABS=$(cd "$(dirname "$BUSYBOX")" && pwd)/$(basename "$BUSYBOX")

# ---- 造一个"已 pull 的镜像"缓存，让 CLI 走真实的镜像分派路径 ----
WORK=$(mktemp -d "${TMPDIR:-/tmp}/wbox-lbe.XXXXXX") || die "mktemp 失败"
trap 'rm -rf "$WORK"' EXIT
CACHE=$WORK/home/.wbox/images/registry-1.docker.io/library_lbetest/latest
mkdir -p "$CACHE/rootfs/bin" "$CACHE/rootfs/proc" "$CACHE/rootfs/etc" "$CACHE/rootfs/mnt"
cp "$BB_ABS" "$CACHE/rootfs/bin/busybox"
chmod +x "$CACHE/rootfs/bin/busybox"
# busybox 按 argv[0] 分派 applet，故给用到的都建符号链接
APPLETS="sh id ls dd sleep echo cat grep mount test true false readlink hostname rm"
for a in $APPLETS; do
  ln -sf busybox "$CACHE/rootfs/bin/$a"
done
# 建完就核对一遍这个 busybox 真的带这些 applet。**这是 harness 自检，不是用例**：
# 缺 applet 时容器里的命令会静默失效，然后某条用例以"容器已退出"之类的样子挂掉——
# 看上去像产品回归，实际是测试环境不全。这类误判在本门禁上反复出现过（判据本身
# 出错而产品无恙），宁可在这里直接 die 说清原因，也不要让它伪装成回归。
for a in $APPLETS; do
  "$BB_ABS" --list 2>/dev/null | grep -qx "$a" \
    || die "busybox（$BB_ABS）不含 applet '$a'：用例会以产品故障的样子失败，请换一个功能更全的静态 busybox"
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

# --max-procs 有两种可接受结局，**按实际发生的事判定，不按宿主特征猜**：
# 早先这里用 `[ -f /sys/fs/cgroup/cgroup.controllers ]` 当"有 cgroup v2"的
# 判据，但那个文件存在**不代表能用**——GitHub runner 上委派没开，实际走的是
# 兜底路径。凡是"探测说有、实际不能用"的机器都会被这种写法误判。
run --max-procs 8 lbetest -- /bin/sh -c 'i=0; while [ $i -lt 40 ]; do /bin/sleep 5 & i=$((i+1)); done; echo all-spawned'
if printf '%s' "$OUT" | grep -qi "can't fork\|Resource temporarily unavailable"; then
  report PASS "L2.2 --max-procs 8 挡住 fork 炸弹"
elif [ "$rc" -eq 1 ] && printf '%s' "$OUT" | grep -q 'max-procs'; then
  report PASS "L2.2 --max-procs 无法实施时明确拒绝（rc=1，不静默放行）"
else
  report FAIL "L2.2 --max-procs" "既没挡住也没拒绝（rc=$rc）：$(printf '%s' "$OUT" | head -c 200)"
fi

# --cpu-pct：要么 cgroup v2 真生效，要么**明确报错**。不接受静默忽略。
run --cpu-pct 50 lbetest -- /bin/id
if [ "$rc" -eq 0 ] && printf '%s' "$OUT" | grep -q 'uid=0'; then
  report PASS "L2.3 --cpu-pct（cgroup v2 路径生效）"
elif [ "$rc" -eq 1 ] && printf '%s' "$OUT" | grep -q 'cpu-pct'; then
  report PASS "L2.3 --cpu-pct 无可用 cgroup v2 时明确拒绝（rc=1，不静默忽略）"
else
  report FAIL "L2.3 --cpu-pct" "既未生效也未明确拒绝（rc=$rc）：$(printf '%s' "$OUT" | head -c 200)"
fi

# 覆盖面留档（不判定成败）：上面 L2.3 走的是哪条路径，决定了这台机器到底
# 有没有覆盖 cgroup v2 首选路径。文档里不能凭"runner 是 cgroup v2"就断言
# 覆盖到了——那正是之前写错的地方。
if [ "$rc" -eq 0 ]; then
  echo "note: 本次运行**覆盖了** cgroup v2 首选路径"
else
  echo "note: 本次运行只覆盖 rlimit 兜底/拒绝路径，cgroup v2 首选路径**未覆盖**"
fi

echo
echo "=== H 台阶①宿主程序模式 / N 网络默认 ==="

# 宿主模式不需要镜像缓存：直接跑宿主已有的程序。
hrun() { OUT=$(HOME=$WORK/home "$WBOX_ABS" run "$@" 2>&1); rc=$?; return $rc; }

hrun -- /bin/sh -c 'echo pid=$$'
if [ "$rc" -eq 0 ] && printf '%s' "$OUT" | grep -q 'pid=1'; then
  report PASS "H.1 宿主程序在新 PID namespace 内运行（PID 1）"
else
  report FAIL "H.1 宿主程序模式" "rc=$rc 输出: $(printf '%s' "$OUT" | head -c 200)"
fi

# 与镜像模式的关键差异：宿主文件系统**照常可见**（这是"环境控制"而非
# "文件系统隔离"，README 已写明 --workdir 不是只读视图）。
hrun -- /bin/ls /
if printf '%s' "$OUT" | grep -qE '^(usr|etc)$'; then
  report PASS "H.2 宿主模式不换根（宿主文件系统可见，与镜像模式相反）"
else
  report FAIL "H.2 宿主模式不换根" "输出: $(printf '%s' "$OUT" | tr '\n' ' ' | head -c 200)"
fi

hrun --workdir /etc -- /bin/pwd
if [ "$rc" -eq 0 ] && [ "$OUT" = /etc ]; then
  report PASS "H.3 --workdir 作为工作目录生效"
else
  report FAIL "H.3 --workdir" "rc=$rc 输出: $OUT（期望 /etc）"
fi

hrun -- /bin/sh -c 'exit 9'
if [ "$rc" -eq 9 ]; then
  report PASS "H.4 宿主模式退出码转发（exit 9）"
else
  report FAIL "H.4 宿主模式退出码转发" "rc=$rc（期望 9）"
fi

# 宿主模式绝不能在宿主 cwd 里留下镜像模式的换根残留物
before=$(ls -a . | wc -l)
hrun -- /bin/true
if [ ! -e .wbox_oldroot ] && [ "$(ls -a . | wc -l)" = "$before" ]; then
  report PASS "H.5 宿主模式不在工作目录留下 .wbox_oldroot/dev 残留"
else
  report FAIL "H.5 宿主模式无残留" "出现了 .wbox_oldroot 或新增文件"
fi

# ---- H.6–H.9：Q4（Linux 上跑 Windows 程序）那一格的能力取证 ----
#
# PRD §2.4 Q4 声称"复用 Q3 同一条 Linux 链路，F9 的身份/capability/seccomp 等
# 对 wine 目标同样生效"。**这个声称必须有门禁**，否则只是一句听着合理的话。
#
# 判据落在**宿主程序模式**上而不是真的调 wine：wine 目标走的正是这条路径
# （wrap_if_pe 只是替换最终 argv，隔离链路一字不差），而宿主模式无需装 wine，
# 因此这组用例在任何机器上都真的会跑，不会退化成 SKIP。
# 只在镜像模式验证是不够的——那证明不了不换根的这条路径也吃到了同样的能力。

hrun --user 1000:1001 -- /bin/sh -c 'id -u; id -g'
if [ "$rc" -eq 0 ] && [ "$(printf '%s' "$OUT" | tr '\n' ' ')" = "1000 1001" ]; then
  report PASS "H.6 宿主模式下 --user 生效（Q4 复用同一链路的取证）"
else
  report FAIL "H.6 宿主模式 --user" "rc=$rc 输出: $(printf '%s' "$OUT" | tr '\n' ' ' | head -c 120)"
fi

hrun --cap-drop ALL -- /bin/sh -c 'grep "^CapEff" /proc/self/status'
capeff=$(printf '%s' "$OUT" | awk '{print $2}')
if [ "$rc" -eq 0 ] && [ "$capeff" = "0000000000000000" ]; then
  report PASS "H.7 宿主模式下 --cap-drop ALL 生效"
else
  report FAIL "H.7 宿主模式 --cap-drop" "rc=$rc CapEff=$capeff（期望全 0）"
fi

hrun --seccomp-deny mount -- /bin/sh -c 'grep "^Seccomp:" /proc/self/status'
secmode=$(printf '%s' "$OUT" | awk '{print $2}')
if [ "$rc" -eq 0 ] && [ "$secmode" = "2" ]; then
  report PASS "H.8 宿主模式下 --seccomp-deny 装载过滤器"
else
  report FAIL "H.8 宿主模式 --seccomp-deny" "rc=$rc Seccomp=$secmode（期望 2）"
fi

# H.9 行为判据：宿主模式下丢掉 SYS_ADMIN 后挂载同样被拒，
# 且不带该选项时同一操作成功——否则这条断言没有意义。
hrun -- /bin/sh -c 'mount -t tmpfs none /mnt 2>/dev/null && echo MOUNT_OK'
h_base=$rc
hrun --cap-drop SYS_ADMIN -- /bin/sh -c 'mount -t tmpfs none /mnt 2>/dev/null && echo MOUNT_OK'
if [ "$h_base" -eq 0 ] && ! printf '%s' "$OUT" | grep -q MOUNT_OK; then
  report PASS "H.9 宿主模式下丢 SYS_ADMIN 后挂载被拒（基线同操作成功）"
elif [ "$h_base" -ne 0 ]; then
  report SKIP "H.9 宿主模式 capability 行为判据" "基线挂载本身就失败（rc=$h_base），无法判定"
else
  report FAIL "H.9 宿主模式 capability 行为判据" "丢掉 SYS_ADMIN 后仍挂载成功"
fi

# ---- H.10–H.12：PRD §2.4 Q4 表里"宿主模式下也能用"的三条声称 ----
#
# stats / pause 只需要进程树，不换根也不需要 cgroup，所以在 wine 那一格照样能用；
# diff / commit / cp 都读 overlay 可写层，宿主模式没有那层，属**不适用**。
# 三条都写进了 PRD 的 Q4 表——写了就要有门禁，否则只是听着合理的话。
HH=$WORK/hostq4
rm -rf "$HH" && mkdir -p "$HH/home"
HOME=$HH/home "$WBOX_ABS" run -d --name hq4 -- /bin/sleep 30 >/dev/null 2>&1
sleep 1

hout=$(HOME=$HH/home "$WBOX_ABS" stats hq4 2>&1); hrc=$?
hmem=$(printf '%s\n' "$hout" | awk '$1=="hq4"{print $3}')
if [ "$hrc" -eq 0 ] && printf '%s' "$hmem" | grep -qE '^[0-9.]+(B|KiB|MiB|GiB|TiB)$'; then
  report PASS "H.10 宿主模式下 stats 可用（Q4 表的声称，内存 $hmem）"
else
  report FAIL "H.10 宿主模式 stats" "rc=$hrc 输出: $(printf '%s' "$hout" | tr '\n' '|' | head -c 160)"
fi

hout=$(HOME=$HH/home "$WBOX_ABS" pause hq4 2>&1); hrc=$?
hout2=$(HOME=$HH/home "$WBOX_ABS" unpause hq4 2>&1); hrc2=$?
if [ "$hrc" -eq 0 ] && [ "$hrc2" -eq 0 ]; then
  report PASS "H.11 宿主模式下 pause/unpause 可用（Q4 表的声称）"
else
  report FAIL "H.11 宿主模式 pause/unpause" "pause rc=$hrc unpause rc=$hrc2"
fi

# 不适用的三个要**说清为什么**。静默给空结果会被读成"没有改动"，
# 那是把"这个问题在这条路径上不存在"说成了"答案是空"。
hd=$(HOME=$HH/home "$WBOX_ABS" diff hq4 2>&1); hdrc=$?
hc=$(HOME=$HH/home "$WBOX_ABS" cp hq4:/etc/hostname "$HH/x" 2>&1); hcrc=$?
if [ "$hdrc" -ne 0 ] && [ "$hcrc" -ne 0 ] \
   && printf '%s' "$hd" | grep -q '宿主程序模式' \
   && printf '%s' "$hc" | grep -q '宿主程序模式'; then
  report PASS "H.12 宿主模式下 diff/cp 明确报'不换根'而非静默给空结果"
else
  report FAIL "H.12 宿主模式 diff/cp 的不适用说明" "diff rc=$hdrc cp rc=$hcrc 输出: $(printf '%s|%s' "$hd" "$hc" | head -c 200)"
fi

HOME=$HH/home "$WBOX_ABS" kill hq4 >/dev/null 2>&1
HOME=$HH/home "$WBOX_ABS" rm hq4 >/dev/null 2>&1
rm -rf "$HH"

if command -v python3 >/dev/null 2>&1; then
  probe="import socket
s=socket.socket(socket.AF_INET,socket.SOCK_DGRAM)
try:
    s.connect(('1.1.1.1',53)); print('NET-OK')
except OSError as e:
    print('NET-BLOCKED', e.errno)"
  hrun -- python3 -c "$probe"
  if printf '%s' "$OUT" | grep -q 'NET-BLOCKED'; then
    report PASS "N.1 默认断网（新建空 network namespace）"
  else
    report FAIL "N.1 默认断网" "输出: $(printf '%s' "$OUT" | head -c 200)"
  fi

  hrun --allow-network -- python3 -c "$probe"
  if printf '%s' "$OUT" | grep -q 'NET-OK'; then
    report PASS "N.2 --allow-network 恢复宿主网络栈"
  else
    # 宿主本身没有外网（离线 runner）时无法判定，不算回归
    report SKIP "N.2 --allow-network" "宿主自身似乎也无外网：$(printf '%s' "$OUT" | head -c 120)"
  fi

  # 新 netns 里 loopback 默认 DOWN，wbox 必须把它拉起来，否则连
  # 127.0.0.1 都不通——那与"不给外网"的意图无关，是附带损伤。
  lo="import socket
s=socket.socket(); s.bind(('127.0.0.1',0)); s.listen(1)
c=socket.socket(); c.connect(s.getsockname()); print('LO-OK')"
  hrun -- python3 -c "$lo"
  if printf '%s' "$OUT" | grep -q 'LO-OK'; then
    report PASS "N.3 断网时 loopback 仍可用（127.0.0.1 可连）"
  else
    report FAIL "N.3 loopback 可用" "输出: $(printf '%s' "$OUT" | head -c 200)"
  fi
else
  report SKIP "N.1/N.2/N.3 网络默认" "宿主无 python3，无法做 socket 探测"
fi

echo
echo "=== C cgroup v2 首选路径（仅在已委派环境）==="

# WBOX_LBE_CGROUP=1 表示"调用方已经把我们放进一个可委派的 cgroup v2 子树"。
# 那时 wbox **必须**走 cgroup 首选路径，退化到 rlimit 就是回归 —— 而不像
# 别处那样两种结局都接受。没设这个变量就 SKIP：绝大多数机器没有可委派 cgroup。
#
# 这一段是为了给首选路径**真正的回归保护**：它一度在任何环境都没执行过，
# 靠"跑通过一次"当作有覆盖，正是本门禁早先踩过的坑。
if [ "${WBOX_LBE_CGROUP:-0}" != 1 ]; then
  report SKIP "C.1-C.2 cgroup v2 首选路径" "未设 WBOX_LBE_CGROUP=1（本环境无可委派 cgroup）"
else
  self_cg=$(sed -n 's/^0:://p' /proc/self/cgroup)
  hrun -V --memory 16 -- /bin/true
  if printf '%s' "$OUT" | grep -q '限额（cgroup v2）'; then
    report PASS "C.1 已委派环境下走 cgroup v2 首选路径（自身 cgroup: ${self_cg:-?}）"
  else
    report FAIL "C.1 cgroup v2 首选路径" \
      "本应走 cgroup 却没走：$(printf '%s' "$OUT" | grep -E '限额' | head -c 200)"
  fi

  # 清理语义：guest 退出后 wbox-<pid> 必须被删掉。旧实现只在失败路径删，
  # 成功路径从不清理，每跑一次漏一个空 cgroup（cgroup v2 不自动回收）。
  guest_cg=$(printf '%s' "$OUT" | sed -n 's/^wbox: cgroup（guest） = //p')
  if [ -z "$guest_cg" ]; then
    report SKIP "C.2 cgroup 目录清理" "没能从 -V 输出里取到 guest cgroup 路径"
  elif [ ! -d "$guest_cg" ]; then
    report PASS "C.2 guest 退出后 cgroup 目录已清理（无泄漏）"
  else
    report FAIL "C.2 cgroup 目录清理" "$guest_cg 仍存在——每跑一次漏一个"
  fi
fi

echo
echo "=== L3 生命周期（进程树收割）==="

# 对齐 Windows 侧 JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE：wbox 被 SIGKILL 后
# 容器内不得留下还在跑的进程。实现是 PR_SET_PDEATHSIG 双段链（wbox → 中间
# 进程 → 新 PID ns 的 PID 1），少任何一段都会漏。
# 判定方式：kill 之前把 wbox 在**宿主视角**下的全部后代 pid 收集起来，
# kill 之后逐个确认它们都没了。不按进程名匹配——名字匹配既会误伤宿主上同名的
# 无关进程，也会漏掉换根后名字不同的情况（先前用重命名的 busybox 当标记，
# 结果 busybox 按 argv[0] 分派 applet，认不出这个名字直接就退了，
# 于是"guest 未起来"而不是"收割成功"）。
descendants() { # $1=根 pid，输出全部后代（含自己）
  local queue="$1" out="" p kids next
  while [ -n "$queue" ]; do
    next=""
    for p in $queue; do
      out="$out $p"
      kids=$(ps -o pid= --ppid "$p" 2>/dev/null | tr -d ' ')
      next="$next $kids"
    done
    queue=$next
  done
  echo $out
}

reap_check() {
  # $1=用例名 $2..=wbox 参数（含 -- 与 guest 命令）
  local name=$1; shift
  HOME=$WORK/home setsid "$WBOX_ABS" run "$@" >/dev/null 2>&1 &
  local wb=$!
  # 从 job 表摘掉，否则 kill -9 之后 bash 会打一行 "Killed ..." 噪音
  disown "$wb" 2>/dev/null || true
  sleep 2
  if ! kill -0 "$wb" 2>/dev/null; then
    report SKIP "$name" "wbox 未能存活到 kill（可能 guest 起不来）"
    return
  fi
  local tree n alive p
  tree=$(descendants "$wb")
  # 期望至少 3 个：wbox + 中间进程 + guest（还有后台的那个 sleep）
  n=$(echo $tree | wc -w)
  if [ "$n" -lt 3 ]; then
    report SKIP "$name" "wbox 后代只有 $n 个（$tree），guest 疑似未起来"
    kill -9 "$wb" 2>/dev/null
    return
  fi
  kill -9 "$wb" 2>/dev/null
  # 轮询到全部消失，而不是"睡 2 秒然后看一眼"。收割是内核的级联
  # （PDEATHSIG → 中间进程 → PID 1 退出 → ns 内清空），机器一忙就可能超过任何
  # 固定等待；用定长 sleep 当判据会造出与产品无关的偶发红灯（实测遇到过一次）。
  # 判据本身没有放松：到期仍有存活照样 FAIL。
  local deadline=$((SECONDS + 10))
  alive=""
  while :; do
    alive=""
    for p in $tree; do
      [ "$p" = "$wb" ] && continue
      kill -0 "$p" 2>/dev/null && alive="$alive $p"
    done
    [ -z "$alive" ] && break
    [ "$SECONDS" -ge "$deadline" ] && break
    sleep 0.2
  done
  if [ -z "$alive" ]; then
    report PASS "$name（kill 前进程树 $n 个，之后全部消失）"
  else
    report FAIL "$name" "SIGKILL wbox 后仍存活：$alive"
    for p in $alive; do kill -9 "$p" 2>/dev/null; done
  fi
}

# 宿主模式：guest 跑宿主自己的 sleep
reap_check "L3.1 宿主模式 wbox 被 SIGKILL 后无残留进程" \
  -- /bin/sh -c '/bin/sleep 300 & /bin/sleep 300'
# 镜像模式：同一条语义必须在换根之后也成立（rootfs 内 sleep 是 busybox 软链）
reap_check "L3.2 镜像模式 wbox 被 SIGKILL 后无残留进程" \
  lbetest -- /bin/sh -c '/bin/sleep 300 & /bin/sleep 300'

echo
echo "=== W 台阶③：Linux 上跑 Windows 程序（集成 wine）==="

# 前置：一个真 PE（用 mingw 现场编）+ 一个 wine 加载器。缺任一记 SKIP——
# 这两样是可选依赖，不像 userns 那样是 wbox 自身的硬前置，故**不**受
# WBOX_LBE_REQUIRE 管辖（专门的 test-wine-backend job 会装齐再跑）。
MINGW=x86_64-w64-mingw32-gcc
wine_found=$(WBOX_WINE=${WBOX_WINE:-} command -v wine64 || command -v wine || \
             ls /usr/lib/wine/wine64 2>/dev/null | head -1)
if ! command -v "$MINGW" >/dev/null 2>&1; then
  report SKIP "W.1-W.4 台阶③" "无 $MINGW，无法现场编 PE 夹具"
elif [ -z "$wine_found" ] && [ -z "${WBOX_WINE:-}" ]; then
  report SKIP "W.1-W.4 台阶③" "本机没有 wine"
else
  cat > "$WORK/hi.c" <<'CEOF'
#include <stdio.h>
int main(int argc, char **argv) {
    printf("PE-OK argc=%d\n", argc);
    return argc == 3 ? 7 : 0;
}
CEOF
  cat > "$WORK/net.c" <<'CEOF'
#include <stdio.h>
#include <winsock2.h>
int main(void) {
    WSADATA w; WSAStartup(MAKEWORD(2,2), &w);
    SOCKET s = socket(AF_INET, SOCK_DGRAM, 0);
    struct sockaddr_in a = {0};
    a.sin_family = AF_INET; a.sin_port = htons(53);
    a.sin_addr.s_addr = inet_addr("1.1.1.1");
    printf(connect(s,(struct sockaddr*)&a,sizeof a) == 0 ? "NET-OK\n" : "NET-BLOCKED\n");
    return 0;
}
CEOF
  if ! "$MINGW" -O2 -o "$WORK/hi.exe" "$WORK/hi.c" 2>"$WORK/cc.log"; then
    report SKIP "W.1-W.4 台阶③" "PE 夹具编译失败：$(head -c 150 "$WORK/cc.log")"
  else
    "$MINGW" -O2 -o "$WORK/net.exe" "$WORK/net.c" -lws2_32 2>/dev/null

    hrun -- "$WORK/hi.exe" a b
    if printf '%s' "$OUT" | grep -q 'PE-OK argc=3'; then
      report PASS "W.1 PE 被自动识别并经 wine 执行（参数原样传入）"
    else
      report FAIL "W.1 跑 PE" "rc=$rc 输出: $(printf '%s' "$OUT" | tail -c 200)"
    fi

    if [ "$rc" -eq 7 ]; then
      report PASS "W.2 Windows 程序退出码原样转发（exit 7）"
    else
      report FAIL "W.2 PE 退出码" "rc=$rc（期望 7）"
    fi

    # 关键：wine 只是执行器变体，隔离语义必须与跑 ELF 时**完全一致**。
    # 这条是 PRD F5/F6 一致性要求的体现——不能"wine 那条路忘了断网"。
    if [ -x "$WORK/net.exe" ]; then
      hrun -- "$WORK/net.exe"
      blocked=$(printf '%s' "$OUT" | grep -c 'NET-BLOCKED')
      hrun --allow-network -- "$WORK/net.exe"
      allowed=$(printf '%s' "$OUT" | grep -c 'NET-OK')
      if [ "$blocked" -ge 1 ] && [ "$allowed" -ge 1 ]; then
        report PASS "W.3 Windows 程序同样默认断网、--allow-network 放行"
      elif [ "$blocked" -ge 1 ]; then
        report SKIP "W.3 PE 网络语义" "默认断网正确，但宿主自身似乎无外网，放行侧无法判定"
      else
        report FAIL "W.3 PE 网络语义" "默认竟未断网：$(printf '%s' "$OUT" | tail -c 150)"
      fi
    else
      report SKIP "W.3 PE 网络语义" "net.exe 编译失败（缺 winsock 头？）"
    fi

    # 镜像模式下的 PE 暂未接线，必须**明确拒绝**而不是让容器内的 shell
    # 去解释它。不拦的话 execvp 遇 ENOEXEC 会回退 /bin/sh，busybox 把 PE 当
    # 脚本读，吐 "MZ...: not found" 加一串语法错误（实测 rc=2，毫无线索）。
    cp "$WORK/hi.exe" "$CACHE/rootfs/bin/hi.exe" 2>/dev/null
    OUT=$(HOME=$WORK/home "$WBOX_ABS" run lbetest -- /bin/hi.exe 2>&1); rc=$?
    if [ "$rc" -eq 1 ] && printf '%s' "$OUT" | grep -q 'PE'; then
      report PASS "W.5 镜像模式下的 PE 明确拒绝（不让 shell 去解释它）"
    else
      report FAIL "W.5 镜像模式 PE 拒绝" "rc=$rc（期望 1）：$(printf '%s' "$OUT" | head -c 200)"
    fi

    # ELF 不能被误塞 wine：误判的代价是拿 wine 去跑 Linux 程序，报错极难懂
    hrun -V -- /bin/echo elf-probe
    if printf '%s' "$OUT" | grep -q '直接执行（目标是本机 ELF）'; then
      report PASS "W.4 ELF 目标不经 wine（PE 判定看完整签名，不只看 MZ）"
    else
      report FAIL "W.4 ELF 误判" "$(printf '%s' "$OUT" | grep 执行器 | head -c 150)"
    fi
  fi
fi

echo
echo "=== R 重启策略（PRD F9.6）==="

# R.1 on-failure:N 要在 N 次之后**真的停下**——否则一个必然失败的容器会无限
# 重启，刷爆日志并空转 CPU。
HOME=$WORK/home "$WBOX_ABS" run -d --name rf --restart on-failure:2 \
  -- /bin/sh -c 'echo ATTEMPT; exit 3' >/dev/null 2>&1
sleep 5
n=$(HOME=$WORK/home "$WBOX_ABS" logs rf 2>/dev/null | grep -c ATTEMPT)
st=$(HOME=$WORK/home "$WBOX_ABS" ps -a 2>/dev/null | grep -c "rf .*exited")
if [ "$n" -eq 3 ] && [ "$st" -eq 1 ]; then
  report PASS "R.1 on-failure:2 重启两次后停下（共 3 次尝试）"
else
  report FAIL "R.1 on-failure 次数" "尝试 $n 次（期望 3），exited=$st"
fi
HOME=$WORK/home "$WBOX_ABS" rm rf >/dev/null 2>&1

# R.2 退出码 0 是"活儿干完了"，on-failure 不该把它当失败
HOME=$WORK/home "$WBOX_ABS" run -d --name rok --restart on-failure \
  -- /bin/sh -c 'echo ONCE; exit 0' >/dev/null 2>&1
sleep 3
n=$(HOME=$WORK/home "$WBOX_ABS" logs rok 2>/dev/null | grep -c ONCE)
if [ "$n" -eq 1 ]; then
  report PASS "R.2 退出码 0 不触发 on-failure 重启"
else
  report FAIL "R.2 成功退出不重启" "跑了 $n 次（期望 1）"
fi
HOME=$WORK/home "$WBOX_ABS" rm rok >/dev/null 2>&1

# R.3 **关键性质**：always 会持续重启，但 `stop` 之后必须不再拉起。
# 这条正是"循环放在 supervisor 里"换来的——stop 终止的就是 supervisor，
# 无需再维护一个"是不是人为停的"标记（那种标记最容易与实际状态不同步）。
HOME=$WORK/home "$WBOX_ABS" run -d --name ral --restart always \
  -- /bin/sh -c 'echo TICK; sleep 1' >/dev/null 2>&1
sleep 4
n1=$(HOME=$WORK/home "$WBOX_ABS" logs ral 2>/dev/null | grep -c TICK)
HOME=$WORK/home "$WBOX_ABS" stop ral >/dev/null 2>&1
sleep 2; n2=$(HOME=$WORK/home "$WBOX_ABS" logs ral 2>/dev/null | grep -c TICK)
sleep 3; n3=$(HOME=$WORK/home "$WBOX_ABS" logs ral 2>/dev/null | grep -c TICK)
if [ "$n1" -gt 1 ] && [ "$n2" -eq "$n3" ]; then
  report PASS "R.3 always 持续重启，stop 后不再拉起（$n1 次 → 停 → $n2=$n3）"
else
  report FAIL "R.3 always/stop" "重启 $n1 次（应 >1）；停后 $n2 → $n3（应相等）"
fi
HOME=$WORK/home "$WBOX_ABS" rm ral >/dev/null 2>&1

# R.4 --restart 与 --rm 语义矛盾，必须报错而非静默让一方胜出
cout=$(HOME=$WORK/home "$WBOX_ABS" run --restart always --rm -- /bin/true 2>&1); crc=$?
if [ "$crc" -ne 0 ] && printf '%s' "$cout" | grep -q "不能同时使用"; then
  report PASS "R.4 --restart 与 --rm 冲突时报错"
else
  report FAIL "R.4 冲突检测" "rc=$crc 输出: $(printf '%s' "$cout" | head -c 150)"
fi

# R.5 管理面必须跟随第二代 PID。第一代写 marker 后失败，第二代保持运行；
# 若 container.pid 仍是第一代，top 会拿不到 /proc 子树。
#
# marker 必须落在 $WORK 下，**不能放宿主根目录**：这条用例是宿主模式（没给
# 镜像参数，故不换根），`/restart-ready` 会落在宿主真实的 `/` 上。那个位置在
# 很多环境里不可写（本仓库的开发容器就是），于是两代都建不出 marker、都
# exit 7，用例以"容器已退出"的样子挂掉——看着像 restart 或 top 回归，
# 实际是写不进去。
rm -f "$WORK/restart-ready"
HOME=$WORK/home "$WBOX_ABS" run -d --name rmanage --restart on-failure:1 --workdir "$WORK" \
  -- /bin/sh -c 'if [ -f restart-ready ]; then sleep 20; else : > restart-ready; exit 7; fi' \
  >/dev/null 2>&1
sleep 3
rmtop=$(HOME=$WORK/home "$WBOX_ABS" top rmanage 2>&1); rmrc=$?
if [ "$rmrc" -eq 0 ] && printf '%s' "$rmtop" | grep -q 'sleep 20'; then
  report PASS "R.5 restart 后 top 跟随新一代 container.pid"
else
  report FAIL "R.5 restart 管理面 PID 刷新" "rc=$rmrc 输出: $(printf '%s' "$rmtop" | head -c 180)"
fi
HOME=$WORK/home "$WBOX_ABS" kill rmanage >/dev/null 2>&1
HOME=$WORK/home "$WBOX_ABS" rm rmanage >/dev/null 2>&1
rm -f "$WORK/restart-ready"

echo
echo "=== B 镜像构建（PRD F9.3）==="

BCTX=$WORK/bctx
mkdir -p "$BCTX"
printf 'HELLO_FROM_COPY\n' > "$BCTX/data.txt"
cat > "$BCTX/Dockerfile" <<'DFEOF'
FROM lbetest:latest
ENV GREETING=built
COPY data.txt /data.txt
RUN /bin/echo RUN_EXECUTED > /ran.txt
CMD ["/bin/cat", "/data.txt", "/ran.txt"]
DFEOF

bout=$(HOME=$WORK/home "$WBOX_ABS" build -t built:v1 "$BCTX" 2>&1); brc=$?
if [ "$brc" -eq 0 ] && printf '%s' "$bout" | grep -q "构建完成"; then
  report PASS "B.1 build 走完 FROM/ENV/COPY/RUN/CMD"
else
  report FAIL "B.1 build" "rc=$brc 输出: $(printf '%s' "$bout" | tr '\n' ' ' | head -c 200)"
fi

# B.2 构建产物必须**能跑**，且 COPY 的文件与 RUN 的副作用都在。
# 只断言"构建成功"是不够的——那只证明流程没报错，不证明镜像是对的。
run built:v1
if [ "$rc" -eq 0 ] && printf '%s' "$OUT" | grep -q HELLO_FROM_COPY \
   && printf '%s' "$OUT" | grep -q RUN_EXECUTED; then
  report PASS "B.2 构建产物可运行（COPY 内容与 RUN 副作用都在）"
else
  report FAIL "B.2 产物可运行" "rc=$rc 输出: $(printf '%s' "$OUT" | tr '\n' ' ' | head -c 200)"
fi

# B.3 **安全断言**：COPY 不得把上下文之外的宿主文件打进镜像。
# 夹具必须用一个**真实存在**的外部文件——本地第一次用了不存在的路径，
# 于是 canonicalize 先失败，报错看着对但越界检查压根没被执行到。
printf 'OUTSIDE_SECRET\n' > "$WORK/outside.txt"
printf 'FROM lbetest:latest\nCOPY ../outside.txt /x\n' > "$BCTX/Dockerfile.esc"
eout=$(HOME=$WORK/home "$WBOX_ABS" build -t esc:v1 -f "$BCTX/Dockerfile.esc" "$BCTX" 2>&1); erc=$?
if [ "$erc" -ne 0 ] && printf '%s' "$eout" | grep -q "逃出了构建上下文"; then
  report PASS "B.3 COPY 拒绝上下文外的源（真实存在的外部文件）"
else
  report FAIL "B.3 COPY 越界拒绝" "rc=$erc 输出: $(printf '%s' "$eout" | head -c 200)"
fi

# B.5 分层缓存：内容未变时重建应命中缓存（否则 build 每次全量重跑，不可用）
b2=$(HOME=$WORK/home "$WBOX_ABS" build -t built:v1 "$BCTX" 2>&1)
if printf '%s' "$b2" | grep -q CACHED; then
  report PASS "B.5 内容未变时重建命中分层缓存"
else
  report FAIL "B.5 缓存命中" "第二次构建未见 CACHED：$(printf '%s' "$b2" | tr '\n' ' ' | head -c 200)"
fi

# B.6 **缓存正确性断言**：改了 COPY 源之后必须失效，产物必须是新内容。
# 这条比"能命中"重要得多——命中一个状态不同的旧层会让构建"成功"却产出错的
# 镜像，是缓存最危险的失效方式，而且完全没有报错信号。
printf 'CHANGED_CONTENT\n' > "$BCTX/data.txt"
HOME=$WORK/home "$WBOX_ABS" build -t built:v1 "$BCTX" >/dev/null 2>&1
run built:v1
if printf '%s' "$OUT" | grep -q CHANGED_CONTENT; then
  report PASS "B.6 改动 COPY 源后缓存正确失效（产物是新内容）"
else
  report FAIL "B.6 缓存失效" "产物仍是旧内容：$(printf '%s' "$OUT" | tr '\n' ' ' | head -c 200)"
fi

# B.4 未实现的指令要报错而不是静默跳过——跳过会产出"看着成功实则少做事"的镜像
printf 'FROM lbetest:latest\nVOLUME /data\n' > "$BCTX/Dockerfile.vol"
vout=$(HOME=$WORK/home "$WBOX_ABS" build -t v:1 -f "$BCTX/Dockerfile.vol" "$BCTX" 2>&1); vrc=$?
if [ "$vrc" -ne 0 ] && printf '%s' "$vout" | grep -q "未实现"; then
  report PASS "B.4 未实现的 Dockerfile 指令明确报错"
else
  report FAIL "B.4 未实现指令报错" "rc=$vrc 输出: $(printf '%s' "$vout" | head -c 150)"
fi

echo
echo "=== OVB 构建期基础层复用（PRD L5b 磁盘侧）==="

# 判据两条，缺一不可：
#   1. 磁盘真省下来（两个镜像共享 inode）；
#   2. **基础镜像缓存逐字节不变**——硬链接共享的前提是没有任何就地改写，
#      纪律破了就会写坏别的镜像，那是最不该引入的一类缺陷，必须直接取证。
OVBCTX=$WORK/ovbctx
mkdir -p "$OVBCTX"
printf 'ADDED\n' > "$OVBCTX/new.txt"
mkdir -p "$CACHE/rootfs/ovbdir" && printf 'indir\n' > "$CACHE/rootfs/ovbdir/x"
printf 'original\n' > "$CACHE/rootfs/ovbkeep.txt"
cat > "$OVBCTX/Dockerfile" <<'OVBEOF'
FROM lbetest
COPY new.txt /new.txt
RUN /bin/sh -c 'echo modified > /ovbkeep.txt; rm -rf /ovbdir; echo made > /ovbrun.txt'
OVBEOF
basesum=$(find "$CACHE/rootfs" -type f -exec sha256sum {} \; | sort | sha256sum)
bout=$(HOME=$WORK/home "$WBOX_ABS" build -t ovbderived:v1 "$OVBCTX" 2>&1); brc=$?
aftersum=$(find "$CACHE/rootfs" -type f -exec sha256sum {} \; | sort | sha256sum)
OVBD=$WORK/home/.wbox/images/registry-1.docker.io/library_ovbderived/v1/rootfs

if [ "$brc" -eq 0 ] && [ "$basesum" = "$aftersum" ] \
   && [ "$(cat "$CACHE/rootfs/ovbkeep.txt")" = "original" ] && [ -d "$CACHE/rootfs/ovbdir" ]; then
  report PASS "OVB.1 构建派生镜像后基础镜像缓存逐字节不变"
else
  report FAIL "OVB.1 基础镜像完整性" "rc=$brc 摘要变了=$([ "$basesum" = "$aftersum" ] && echo 否 || echo 是) keep=$(cat "$CACHE/rootfs/ovbkeep.txt" 2>/dev/null)"
fi

# 产物内容：COPY 新增、RUN 改写、RUN 删目录，三样都要对
if [ "$(cat "$OVBD/new.txt" 2>/dev/null)" = "ADDED" ] \
   && [ "$(cat "$OVBD/ovbkeep.txt" 2>/dev/null)" = "modified" ] \
   && [ "$(cat "$OVBD/ovbrun.txt" 2>/dev/null)" = "made" ] \
   && [ ! -d "$OVBD/ovbdir" ]; then
  report PASS "OVB.2 产物内容正确（COPY 新增 / RUN 改写 / RUN 删目录都生效）"
else
  report FAIL "OVB.2 产物内容" "new=$(cat "$OVBD/new.txt" 2>/dev/null) keep=$(cat "$OVBD/ovbkeep.txt" 2>/dev/null) run=$(cat "$OVBD/ovbrun.txt" 2>/dev/null) 目录仍在=$([ -d "$OVBD/ovbdir" ] && echo 是 || echo 否)"
fi

# 磁盘：合计（共享 inode 只算一次）必须明显小于两份之和
b1=$(du -s "$CACHE/rootfs" | cut -f1)
b2=$(du -s "$OVBD" | cut -f1)
both=$(du -sc "$CACHE/rootfs" "$OVBD" | tail -1 | cut -f1)
sum=$((b1 + b2))
i1=$(stat -c '%i' "$CACHE/rootfs/bin/busybox" 2>/dev/null)
i2=$(stat -c '%i' "$OVBD/bin/busybox" 2>/dev/null)
if [ -n "$i1" ] && [ "$i1" = "$i2" ] && [ "$both" -lt $((sum * 3 / 4)) ]; then
  report PASS "OVB.3 磁盘共享生效（合计 ${both}KB，两份之和 ${sum}KB；未改文件 inode 相同）"
else
  report FAIL "OVB.3 磁盘共享" "合计=${both}KB 两份之和=${sum}KB inode $i1 vs $i2"
fi

# 改过的文件必须已断开硬链接，否则就是写到了基础镜像里
k1=$(stat -c '%i' "$CACHE/rootfs/ovbkeep.txt" 2>/dev/null)
k2=$(stat -c '%i' "$OVBD/ovbkeep.txt" 2>/dev/null)
if [ -n "$k1" ] && [ "$k1" != "$k2" ]; then
  report PASS "OVB.4 被 RUN 改写的文件已断开硬链接（写入未回流基础镜像）"
else
  report FAIL "OVB.4 写时断链" "inode $k1 vs $k2（相同说明就地改写了共享 inode）"
fi

HOME=$WORK/home "$WBOX_ABS" rmi ovbderived:v1 >/dev/null 2>&1
rm -rf "$CACHE/rootfs/ovbdir" "$CACHE/rootfs/ovbkeep.txt"

echo
echo "=== V 卷挂载（PRD F9.1）==="

VOLSRC=$WORK/volsrc
mkdir -p "$VOLSRC"
printf 'FROM_HOST\n' > "$VOLSRC/marker.txt"

# V.1 读写挂载：容器要能读到宿主文件，写回也要在宿主侧可见
run -v "$VOLSRC:/data" lbetest -- /bin/sh -c 'cat /data/marker.txt && echo BACK > /data/back.txt'
if [ "$rc" -eq 0 ] && printf '%s' "$OUT" | grep -q FROM_HOST && [ -f "$VOLSRC/back.txt" ]; then
  report PASS "V.1 -v 读写挂载（宿主↔容器双向可见）"
else
  report FAIL "V.1 -v 读写挂载" "rc=$rc 输出: $(printf '%s' "$OUT" | head -c 150) back.txt=$([ -f "$VOLSRC/back.txt" ] && echo yes || echo no)"
fi

# V.2 只读挂载必须真的只读。这条专盯 mount(2) 的一个坑：首次 bind 会**忽略**
# MS_RDONLY，必须再 remount 一次才生效——漏了这步 :ro 会静默变成可写，
# 那比不支持只读更糟（用户以为数据受保护）。
run -v "$VOLSRC:/ro:ro" lbetest -- /bin/sh -c 'echo X > /ro/should-fail.txt'
if [ "$rc" -ne 0 ] && [ ! -f "$VOLSRC/should-fail.txt" ]; then
  report PASS "V.2 -v :ro 真的只读（写被拒且宿主无新文件）"
else
  report FAIL "V.2 :ro 只读" "rc=$rc（期望非 0）；宿主是否被写入=$([ -f "$VOLSRC/should-fail.txt" ] && echo 是 || echo 否)"
fi

# V.3 安全断言：挂到容器根会让隔离作废，必须拒绝
vout=$(HOME=$WORK/home "$WBOX_ABS" run -v "$VOLSRC:/" lbetest -- /bin/true 2>&1); vrc=$?
if [ "$vrc" -ne 0 ] && printf '%s' "$vout" | grep -q "隔离失效"; then
  report PASS "V.3 拒绝 -v 挂载到容器根 '/'"
else
  report FAIL "V.3 拒绝挂载到根" "rc=$vrc 输出: $(printf '%s' "$vout" | head -c 150)"
fi

# V.4 宿主路径不存在要报错，**不能**替用户创建——拼错的路径会变成空目录，
# 等发现数据"不见了"才知道挂错了。
vout=$(HOME=$WORK/home "$WBOX_ABS" run -v "$WORK/definitely-absent:/d" lbetest -- /bin/true 2>&1); vrc=$?
if [ "$vrc" -ne 0 ] && [ ! -e "$WORK/definitely-absent" ]; then
  report PASS "V.4 宿主路径不存在时报错且不自动创建"
else
  report FAIL "V.4 不存在的宿主路径" "rc=$vrc；是否被创建=$([ -e "$WORK/definitely-absent" ] && echo 是 || echo 否)"
fi

echo
echo "=== U --user 身份（PRD F9.7）==="

# U.1 默认仍是容器内 root——`--user` 不能悄悄改掉既有默认。
run lbetest -- /bin/sh -c 'id -u; id -g'
if [ "$rc" -eq 0 ] && [ "$(printf '%s' "$OUT" | tr '\n' ' ')" = "0 0" ]; then
  report PASS "U.1 不带 --user 时默认仍为 uid=0 gid=0"
else
  report FAIL "U.1 默认身份" "rc=$rc 输出: $(printf '%s' "$OUT" | tr '\n' ' ' | head -c 150)"
fi

# U.2 单值形式：gid 跟随 uid（与 docker 一致）。
run --user 1000 lbetest -- /bin/sh -c 'id -u; id -g'
if [ "$rc" -eq 0 ] && [ "$(printf '%s' "$OUT" | tr '\n' ' ')" = "1000 1000" ]; then
  report PASS "U.2 --user 1000（gid 跟随 uid）"
else
  report FAIL "U.2 --user 1000" "rc=$rc 输出: $(printf '%s' "$OUT" | tr '\n' ' ' | head -c 150)"
fi

# U.3 uid:gid 分别指定。这条同时验证了实现路线：rootless 下没有 newuidmap，
# 只能映射**一个** id，所以做法是把宿主那唯一的 id 直接映射成目标号，
# 而不是"先当 root 再 setuid"（后者需要第二条映射，无特权时写不进去）。
run --user 1000:1001 lbetest -- /bin/sh -c 'id -u; id -g'
if [ "$rc" -eq 0 ] && [ "$(printf '%s' "$OUT" | tr '\n' ' ')" = "1000 1001" ]; then
  report PASS "U.3 --user 1000:1001（uid/gid 分别生效）"
else
  report FAIL "U.3 --user 1000:1001" "rc=$rc 输出: $(printf '%s' "$OUT" | tr '\n' ' ' | head -c 150)"
fi

# U.4 用户名要报错而不是静默当成 0——静默降级会让用户以为已经降权了。
uout=$(HOME=$WORK/home "$WBOX_ABS" run --user nobody lbetest -- /bin/true 2>&1); urc=$?
if [ "$urc" -ne 0 ] && printf '%s' "$uout" | grep -q '/etc/passwd'; then
  report PASS "U.4 拒绝用户名并解释原因"
else
  report FAIL "U.4 拒绝用户名" "rc=$urc 输出: $(printf '%s' "$uout" | head -c 150)"
fi

echo
echo "=== CAP capability 裁剪（PRD F9.8）==="

# CAP.1 默认保留全部——引入这个开关不能悄悄收紧既有容器。
run lbetest -- /bin/sh -c 'grep "^CapEff" /proc/self/status'
capdefault=$(printf '%s' "$OUT" | awk '{print $2}')
if [ "$rc" -eq 0 ] && [ -n "$capdefault" ] && [ "$capdefault" != "0000000000000000" ]; then
  report PASS "CAP.1 默认保留全部 capability（CapEff=$capdefault）"
else
  report FAIL "CAP.1 默认 capability" "rc=$rc 输出: $(printf '%s' "$OUT" | head -c 150)"
fi

# CAP.2 --cap-drop ALL 后 CapEff 与 CapBnd 都必须清零。两个都要看：只清
# CapEff 不动 bounding set 的话，execve 之后还能重新拿回来，等于没丢。
run --cap-drop ALL lbetest -- /bin/sh -c 'grep -E "^Cap(Eff|Bnd)" /proc/self/status'
capeff=$(printf '%s' "$OUT" | awk '/CapEff/{print $2}')
capbnd=$(printf '%s' "$OUT" | awk '/CapBnd/{print $2}')
if [ "$rc" -eq 0 ] && [ "$capeff" = "0000000000000000" ] && [ "$capbnd" = "0000000000000000" ]; then
  report PASS "CAP.2 --cap-drop ALL 清空 CapEff 与 CapBnd"
else
  report FAIL "CAP.2 --cap-drop ALL" "rc=$rc CapEff=$capeff CapBnd=$capbnd（都应为全 0）"
fi

# CAP.3 先 drop 后 add（docker 的顺序）：只应剩下 NET_RAW（位 13 = 0x2000）。
run --cap-drop ALL --cap-add NET_RAW lbetest -- /bin/sh -c 'grep "^CapEff" /proc/self/status'
capeff=$(printf '%s' "$OUT" | awk '{print $2}')
if [ "$rc" -eq 0 ] && [ "$capeff" = "0000000000002000" ]; then
  report PASS "CAP.3 --cap-drop ALL --cap-add NET_RAW 只保留 NET_RAW"
else
  report FAIL "CAP.3 drop ALL + add NET_RAW" "rc=$rc CapEff=$capeff（期望 0000000000002000）"
fi

# CAP.4 **行为**判据，不只看位图。位图对了不代表内核真的按它执法；
# 这条直接验证丢掉 CAP_SYS_ADMIN 之后容器内确实挂不了 tmpfs，
# 且不带该选项时同一操作是成功的（否则这条断言毫无意义）。
run lbetest -- /bin/sh -c 'mount -t tmpfs none /mnt && echo MOUNT_OK'
mount_base=$rc
run --cap-drop SYS_ADMIN lbetest -- /bin/sh -c 'mount -t tmpfs none /mnt && echo MOUNT_OK'
if [ "$mount_base" -eq 0 ] && [ "$rc" -ne 0 ]; then
  report PASS "CAP.4 丢掉 SYS_ADMIN 后容器内挂载被拒（无该选项时同操作成功）"
elif [ "$mount_base" -ne 0 ]; then
  report SKIP "CAP.4 SYS_ADMIN 行为判据" "基线挂载本身就失败（rc=$mount_base），无法判定"
else
  report FAIL "CAP.4 SYS_ADMIN 行为判据" "丢掉 SYS_ADMIN 后仍挂载成功（rc=$rc）"
fi

# CAP.5 拼错的名字必须报错。静默忽略会让用户以为已经收紧了，
# 那比不支持这个选项更危险。
cout=$(HOME=$WORK/home "$WBOX_ABS" run --cap-drop NOPE lbetest -- /bin/true 2>&1); crc=$?
if [ "$crc" -ne 0 ] && printf '%s' "$cout" | grep -q "未知 capability"; then
  report PASS "CAP.5 未知 capability 名报错而非静默忽略"
else
  report FAIL "CAP.5 未知名字" "rc=$crc 输出: $(printf '%s' "$cout" | head -c 150)"
fi

echo
echo "=== SEC seccomp 拦截（PRD F9.9）==="

# SEC.1 不配就不装过滤器。/proc/self/status 的 Seccomp 字段 0=disabled 2=filter。
run lbetest -- /bin/sh -c 'grep "^Seccomp:" /proc/self/status'
secmode=$(printf '%s' "$OUT" | awk '{print $2}')
if [ "$rc" -eq 0 ] && [ "$secmode" = "0" ]; then
  report PASS "SEC.1 不配 --seccomp-deny 时不装过滤器（Seccomp=0）"
else
  report FAIL "SEC.1 默认不装过滤器" "rc=$rc Seccomp=$secmode（期望 0）"
fi

# SEC.2 配了就必须真的装上（Seccomp=2 = filter 模式）。
run --seccomp-deny ptrace lbetest -- /bin/sh -c 'grep "^Seccomp:" /proc/self/status'
secmode=$(printf '%s' "$OUT" | awk '{print $2}')
if [ "$rc" -eq 0 ] && [ "$secmode" = "2" ]; then
  report PASS "SEC.2 --seccomp-deny 后进入 filter 模式（Seccomp=2）"
else
  report FAIL "SEC.2 装载过滤器" "rc=$rc Seccomp=$secmode（期望 2）"
fi

# SEC.3 **行为**判据。位模式对不代表拦对了 syscall 号——跳转偏移算错就会
# 落到 ALLOW 上，表现成"配了却没拦住"。这条直接看 mount 有没有被挡，
# 且基线（不配时）同一操作必须成功，否则断言没有意义。
run lbetest -- /bin/sh -c 'mount -t tmpfs none /mnt && echo MOUNT_OK'
sec_base=$rc
run --seccomp-deny mount lbetest -- /bin/sh -c 'mount -t tmpfs none /mnt && echo MOUNT_OK'
if [ "$sec_base" -eq 0 ] && ! printf '%s' "$OUT" | grep -q MOUNT_OK; then
  report PASS "SEC.3 --seccomp-deny mount 实际拦住挂载（基线同操作成功）"
elif [ "$sec_base" -ne 0 ]; then
  report SKIP "SEC.3 mount 行为判据" "基线挂载本身就失败（rc=$sec_base），无法判定"
else
  report FAIL "SEC.3 mount 行为判据" "配了却仍挂载成功: $(printf '%s' "$OUT" | head -c 150)"
fi

# SEC.4 裸号是逃生口：名表刻意不求全，写号必须等价于写名字。
# syscall 号按架构而定，shell 里没有可移植的查法，故只在 x86-64 上断言
# （mount = 165），其余架构记 SKIP —— 宁可不测，也不要用一个可能错的号
# 造出假失败。
run --seccomp-deny mount lbetest -- /bin/sh -c 'mount -t tmpfs none /mnt 2>/dev/null || echo DENIED'
byname=$OUT
if [ "$(uname -m)" = "x86_64" ]; then
  run --seccomp-deny 165 lbetest -- /bin/sh -c 'mount -t tmpfs none /mnt 2>/dev/null || echo DENIED'
  if printf '%s' "$byname" | grep -q DENIED && printf '%s' "$OUT" | grep -q DENIED; then
    report PASS "SEC.4 写 syscall 号与写名字等价（逃生口可用）"
  else
    report FAIL "SEC.4 裸号逃生口" "按名字=$(printf '%s' "$byname"|head -c 60) 按号=$(printf '%s' "$OUT"|head -c 60)"
  fi
else
  report SKIP "SEC.4 裸号逃生口" "syscall 号随架构而变，本用例只在 x86-64 上断言（当前 $(uname -m)）"
fi

# SEC.5 拒绝 execve 等于拒绝启动容器自己，必须在参数层就说清楚，
# 不能让用户只看到一句 "Operation not permitted"。
sout=$(HOME=$WORK/home "$WBOX_ABS" run --seccomp-deny execve lbetest -- /bin/true 2>&1); src=$?
if [ "$src" -ne 0 ] && printf '%s' "$sout" | grep -q "启动容器本身"; then
  report PASS "SEC.5 拒绝 execve 的自毁配置被挡下并解释原因"
else
  report FAIL "SEC.5 自毁配置" "rc=$src 输出: $(printf '%s' "$sout" | head -c 150)"
fi

# SEC.6 拼错的名字要报错，并指出可以直接写号。
sout=$(HOME=$WORK/home "$WBOX_ABS" run --seccomp-deny ptraze lbetest -- /bin/true 2>&1); src=$?
if [ "$src" -ne 0 ] && printf '%s' "$sout" | grep -q "未知 syscall"; then
  report PASS "SEC.6 未知 syscall 名报错而非静默忽略"
else
  report FAIL "SEC.6 未知名字" "rc=$src 输出: $(printf '%s' "$sout" | head -c 150)"
fi

echo
echo "=== HC 健康检查（PRD F9.10）==="

# HC.1 探针成功 → ps 的状态列显示 healthy。
HOME=$WORK/home "$WBOX_ABS" run -d --name hc1 --health-cmd 'true' --health-interval 1 \
  lbetest -- /bin/sleep 20 >/dev/null 2>&1
sleep 3
hout=$(HOME=$WORK/home "$WBOX_ABS" ps 2>&1)
if printf '%s' "$hout" | grep -q 'hc1.*running (healthy)'; then
  report PASS "HC.1 探针成功时 ps 显示 healthy"
else
  report FAIL "HC.1 healthy" "输出: $(printf '%s' "$hout" | tr '\n' ' ' | head -c 200)"
fi

# HC.2 连续失败到达 retries → unhealthy。
HOME=$WORK/home "$WBOX_ABS" run -d --name hc2 --health-cmd 'false' --health-interval 1 \
  --health-retries 2 lbetest -- /bin/sleep 20 >/dev/null 2>&1
sleep 4
hout=$(HOME=$WORK/home "$WBOX_ABS" ps 2>&1)
if printf '%s' "$hout" | grep -q 'hc2.*running (unhealthy)'; then
  report PASS "HC.2 连续失败到达 --health-retries 后判 unhealthy"
else
  report FAIL "HC.2 unhealthy" "输出: $(printf '%s' "$hout" | tr '\n' ' ' | head -c 200)"
fi

# HC.3 **这条是整个功能的成立前提**：探针必须跑在容器里，不是宿主上。
# 判据要能区分两者，否则测了等于没测：/home 在宿主存在、在测试 rootfs 里不存在。
# 探针若跑在宿主上会 healthy；跑在容器里必须 unhealthy。
if [ -d /home ]; then
  HOME=$WORK/home "$WBOX_ABS" run -d --name hc3 --health-cmd 'test -d /home' \
    --health-interval 1 --health-retries 1 lbetest -- /bin/sleep 20 >/dev/null 2>&1
  sleep 3
  hout=$(HOME=$WORK/home "$WBOX_ABS" ps 2>&1)
  if printf '%s' "$hout" | grep -q 'hc3.*running (unhealthy)'; then
    report PASS "HC.3 探针跑在容器内而非宿主（宿主有 /home，rootfs 没有）"
  else
    report FAIL "HC.3 探针执行位置" "探到了宿主的 /home？输出: $(printf '%s' "$hout" | tr '\n' ' ' | head -c 200)"
  fi
  HOME=$WORK/home "$WBOX_ABS" kill hc3 >/dev/null 2>&1
  HOME=$WORK/home "$WBOX_ABS" rm hc3 >/dev/null 2>&1
else
  report SKIP "HC.3 探针执行位置" "宿主没有 /home，这个判据区分不了容器与宿主"
fi

for c in hc1 hc2; do
  HOME=$WORK/home "$WBOX_ABS" kill "$c" >/dev/null 2>&1
  HOME=$WORK/home "$WBOX_ABS" rm "$c" >/dev/null 2>&1
done

# HC.4 调参选项单独出现时必须报错：没有 --health-cmd 就没有探针可跑，
# 静默接受会让用户以为配好了。
hout=$(HOME=$WORK/home "$WBOX_ABS" run --health-interval 5 lbetest -- /bin/true 2>&1); hrc=$?
if [ "$hrc" -ne 0 ] && printf '%s' "$hout" | grep -q "需要与 --health-cmd 同用"; then
  report PASS "HC.4 --health-interval 单独出现时报错"
else
  report FAIL "HC.4 缺 --health-cmd" "rc=$hrc 输出: $(printf '%s' "$hout" | head -c 150)"
fi

# HC.5 interval=0 会把探针变成忙循环，必须挡下并说清原因。
hout=$(HOME=$WORK/home "$WBOX_ABS" run --health-cmd true --health-interval 0 lbetest -- /bin/true 2>&1); hrc=$?
if [ "$hrc" -ne 0 ] && printf '%s' "$hout" | grep -q "忙循环"; then
  report PASS "HC.5 --health-interval 0 被挡下（否则探针空转占满 CPU）"
else
  report FAIL "HC.5 interval=0" "rc=$hrc 输出: $(printf '%s' "$hout" | head -c 150)"
fi

echo
echo "=== N2 端口转发（PRD F9.2）==="

# 判据说明：容器内起一个 TCP 监听，宿主侧连 -p 指定的端口。用 python3 起监听
# ——busybox 的 nc 各版本行为不一，而这条要测的是转发链路，不该被 applet 差异
# 干扰。没有 python3 就整组 SKIP。
if ! command -v python3 >/dev/null 2>&1; then
  absent "N2 端口转发" "缺 python3（容器内监听夹具）"
else
LISTEN_PY='
import socket
s=socket.socket(); s.setsockopt(socket.SOL_SOCKET,socket.SO_REUSEADDR,1)
s.bind(("127.0.0.1",9099)); s.listen(5)
while True:
    c,_=s.accept(); c.sendall(b"HELLO_FROM_CONTAINER\n"); c.close()
'
HOME=$WORK/home "$WBOX_ABS" run -d --name netfwd -p 18099:9099 \
  -- python3 -c "$LISTEN_PY" >/dev/null 2>&1
sleep 3
got=$(timeout 6 python3 -c "
import socket
try:
    s=socket.create_connection(('127.0.0.1',18099),timeout=4)
    print(s.recv(100).decode().strip())
except Exception as e:
    print('ERR:%s' % type(e).__name__)
" 2>&1)
if printf '%s' "$got" | grep -q HELLO_FROM_CONTAINER; then
  report PASS "N2.1 -p 把宿主端口转发进容器 netns"
else
  report FAIL "N2.1 -p 转发" "宿主侧收到: $(printf '%s' "$got" | head -c 120)"
fi
HOME=$WORK/home "$WBOX_ABS" stop netfwd >/dev/null 2>&1
HOME=$WORK/home "$WBOX_ABS" rm netfwd >/dev/null 2>&1

# N2.2 **安全断言**：不给 -p 时容器端口不得从宿主可达。转发能用不等于隔离还在，
# 这条要确认默认仍是隔离的——否则 -p 就成了"顺手把容器网络暴露出去"。
HOME=$WORK/home "$WBOX_ABS" run -d --name noport \
  -- python3 -c "$LISTEN_PY" >/dev/null 2>&1
sleep 2
leak=$(timeout 5 python3 -c "
import socket
try:
    s=socket.create_connection(('127.0.0.1',9099),timeout=2); print('LEAK')
except Exception: print('ISOLATED')
" 2>&1)
if printf '%s' "$leak" | grep -q ISOLATED; then
  report PASS "N2.2 未给 -p 时容器端口不从宿主可达（默认仍隔离）"
else
  report FAIL "N2.2 默认隔离" "宿主竟能连到容器端口：$leak"
fi
HOME=$WORK/home "$WBOX_ABS" stop noport >/dev/null 2>&1
HOME=$WORK/home "$WBOX_ABS" rm noport >/dev/null 2>&1
fi

# N2.3 -p 与 --allow-network 语义冲突，必须报错而不是静默二选一
cout=$(HOME=$WORK/home "$WBOX_ABS" run -p 1:1 --allow-network -- /bin/true 2>&1); crc=$?
if [ "$crc" -ne 0 ] && printf '%s' "$cout" | grep -q "不能同时使用"; then
  report PASS "N2.3 -p 与 --allow-network 冲突时报错"
else
  report FAIL "N2.3 冲突检测" "rc=$crc 输出: $(printf '%s' "$cout" | head -c 150)"
fi

echo
echo "=== NC 容器间共享网络（PRD F9.11）==="

# NC.1 加入方与目标必须在**同一个** netns（inode 相同），且不是宿主的。
# 三方比对缺一不可：只比 A=B 会漏掉"两者都错进了宿主网络"的情况。
HOME=$WORK/home "$WBOX_ABS" run -d --name ncpeer -- /bin/sleep 30 >/dev/null 2>&1
sleep 1
nsA=$(timeout 10 env HOME=$WORK/home "$WBOX_ABS" exec ncpeer -- readlink /proc/self/ns/net 2>/dev/null)
nsB=$(timeout 10 env HOME=$WORK/home "$WBOX_ABS" run --rm --name ncjoin --network container:ncpeer -- readlink /proc/self/ns/net 2>/dev/null)
nsH=$(readlink /proc/self/ns/net)
if [ -n "$nsA" ] && [ "$nsA" = "$nsB" ] && [ "$nsA" != "$nsH" ]; then
  report PASS "NC.1 --network container: 加入同一 netns（且非宿主网络）"
else
  report FAIL "NC.1 netns 一致性" "peer=$nsA joiner=$nsB host=$nsH"
fi

# NC.2 **行为**判据：两容器经 localhost 真的互通。目标容器里起 TCP 监听，
# 加入方连 127.0.0.1——这正是 docker `--network container:` 的核心用途
# （sidecar 模式）。宿主模式容器可见宿主文件系统，python3 直接可用。
if command -v python3 >/dev/null 2>&1; then
  NC_LISTEN='
import socket
s=socket.socket(); s.setsockopt(socket.SOL_SOCKET,socket.SO_REUSEADDR,1)
s.bind(("127.0.0.1",9188)); s.listen(5)
while True:
    c,_=s.accept(); c.sendall(b"PEER_SAYS_HI\n"); c.close()
'
  HOME=$WORK/home "$WBOX_ABS" run -d --name ncsrv -- python3 -c "$NC_LISTEN" >/dev/null 2>&1
  sleep 2
  got=$(timeout 10 env HOME=$WORK/home "$WBOX_ABS" run --rm --name nccli --network container:ncsrv -- python3 -c "
import socket
try:
    s=socket.create_connection(('127.0.0.1',9188),timeout=4)
    print(s.recv(100).decode().strip())
except Exception as e:
    print('ERR:%s' % type(e).__name__)
" 2>&1)
  if printf '%s' "$got" | grep -q PEER_SAYS_HI; then
    report PASS "NC.2 两容器经 localhost 互通（sidecar 模式成立）"
  else
    report FAIL "NC.2 localhost 互通" "加入方收到: $(printf '%s' "$got" | head -c 120)"
  fi
  HOME=$WORK/home "$WBOX_ABS" stop ncsrv >/dev/null 2>&1
  HOME=$WORK/home "$WBOX_ABS" rm ncsrv >/dev/null 2>&1
else
  absent "NC.2 localhost 互通" "缺 python3（容器内监听夹具）"
fi

# NC.3 三种组合冲突都要在参数层报错：网络来源、端口发布位置、身份映射
# 各只能有一个出处，静默让一方胜出会把另一方变成谎言。
ncout=$(HOME=$WORK/home "$WBOX_ABS" run --network container:ncpeer -p 8080:80 -- /bin/true 2>&1); ncrc=$?
if [ "$ncrc" -ne 0 ] && printf '%s' "$ncout" | grep -q "拥有网络的容器"; then
  report PASS "NC.3a --network container: 与 -p 冲突时报错"
else
  report FAIL "NC.3a -p 冲突" "rc=$ncrc 输出: $(printf '%s' "$ncout" | head -c 150)"
fi
ncout=$(HOME=$WORK/home "$WBOX_ABS" run --network container:ncpeer --user 5 -- /bin/true 2>&1); ncrc=$?
if [ "$ncrc" -ne 0 ] && printf '%s' "$ncout" | grep -q "user namespace"; then
  report PASS "NC.3b --network container: 与 --user 冲突时报错"
else
  report FAIL "NC.3b --user 冲突" "rc=$ncrc 输出: $(printf '%s' "$ncout" | head -c 150)"
fi
ncout=$(HOME=$WORK/home "$WBOX_ABS" run --network container:ncpeer --allow-network -- /bin/true 2>&1); ncrc=$?
if [ "$ncrc" -ne 0 ] && printf '%s' "$ncout" | grep -q "网络只能来自一个地方"; then
  report PASS "NC.3c --network container: 与 --allow-network 冲突时报错"
else
  report FAIL "NC.3c --allow-network 冲突" "rc=$ncrc 输出: $(printf '%s' "$ncout" | head -c 150)"
fi

# NC.4 目标已退出时要明确拒绝——那时没有网络可加入，静默起一个断网容器
# 会让 sidecar 悄悄失联。
HOME=$WORK/home "$WBOX_ABS" kill ncpeer >/dev/null 2>&1
sleep 1
ncout=$(HOME=$WORK/home "$WBOX_ABS" run --network container:ncpeer -- /bin/true 2>&1); ncrc=$?
if [ "$ncrc" -ne 0 ] && printf '%s' "$ncout" | grep -q "已退出"; then
  report PASS "NC.4 目标容器已退出时明确拒绝"
else
  report FAIL "NC.4 已退出的目标" "rc=$ncrc 输出: $(printf '%s' "$ncout" | head -c 150)"
fi
HOME=$WORK/home "$WBOX_ABS" rm ncpeer >/dev/null 2>&1

echo
echo "=== OV overlay 可写层（PRD F9.12）==="

# OV.1 核心断言：容器对 / 的写入自己可见，但**不落进共享镜像缓存**。
# 这是 docker 语义（写入永不碰镜像）；改造前 wbox 是共享写入，R.5 调试时
# 亲眼见过 guest 写 / 落进 $CACHE/rootfs。
run lbetest -- /bin/sh -c 'echo dirty > /ov-probe && cat /ov-probe'
if [ "$rc" -eq 0 ] && printf '%s' "$OUT" | grep -q dirty && [ ! -e "$CACHE/rootfs/ov-probe" ]; then
  report PASS "OV.1 容器写 / 自己可见且不污染镜像缓存"
else
  report FAIL "OV.1 写入隔离" "rc=$rc 输出:$(printf '%s' "$OUT"|head -c 80) 缓存被污染=$([ -e "$CACHE/rootfs/ov-probe" ] && echo 是 || echo 否)"
fi

# OV.2 lower（镜像原有内容）必须透过合并视图可见——只测"写不进去"不够，
# 挂错 lower 的表现恰恰是"读不到镜像内容"。
run lbetest -- /bin/ls /bin
if [ "$rc" -eq 0 ] && printf '%s' "$OUT" | grep -q '^busybox$'; then
  report PASS "OV.2 镜像原有内容透过 overlay 可见"
else
  report FAIL "OV.2 lower 可见性" "rc=$rc 输出: $(printf '%s' "$OUT" | tr '\n' ' ' | head -c 120)"
fi

# OV.3 detached 容器退出后 upper 保留——能翻"它到底改了什么"，这是 overlay
# 相对共享写入白得的排障能力；rm 后随状态目录一并清掉。
HOME=$WORK/home "$WBOX_ABS" run -d --name ovd lbetest -- /bin/sh -c 'echo trace > /left-behind' >/dev/null 2>&1
sleep 2
if [ -f "$WORK/home/.wbox/run/ovd/layer/upper/left-behind" ]; then
  report PASS "OV.3 detached 退出后 upper 保留可查（rm 前）"
else
  report FAIL "OV.3 upper 保留" "$(ls "$WORK/home/.wbox/run/ovd/layer/upper" 2>&1 | head -c 120)"
fi
HOME=$WORK/home "$WBOX_ABS" rm ovd >/dev/null 2>&1
if [ ! -e "$WORK/home/.wbox/run/ovd" ]; then
  report PASS "OV.4 rm 连同 overlay 层一并清理"
else
  report FAIL "OV.4 层清理" "状态目录仍在"
fi

# OV.5 WBOX_NO_OVERLAY=1 逃生口回退共享写入（并留下可见的行为差异）。
# 写进缓存后立即清掉，别让残留影响后续用例。
OUT=$(HOME=$WORK/home WBOX_NO_OVERLAY=1 "$WBOX_ABS" run --rm --name ovl lbetest -- /bin/sh -c 'echo old > /legacy-way' 2>&1); rc=$?
if [ "$rc" -eq 0 ] && [ -f "$CACHE/rootfs/legacy-way" ]; then
  report PASS "OV.5 WBOX_NO_OVERLAY=1 回退共享写入（逃生口可用）"
else
  report FAIL "OV.5 逃生口" "rc=$rc 缓存内=$([ -f "$CACHE/rootfs/legacy-way" ] && echo 有 || echo 无)"
fi
rm -f "$CACHE/rootfs/legacy-way" "$CACHE/rootfs/ov-probe"

# OV.6 容器内**删除镜像自带的目录**必须成功，且不动到共享缓存。
# 这条盯的是 rootless overlay 的 `userxattr`：不加它时 overlay 要写
# trusted.overlay.* xattr 标记 opaque 目录，而那需要初始 userns 的
# CAP_SYS_ADMIN——rootless 拿不到，于是 `rm -rf` 一个 lower 里的目录直接
# EIO 失败。**删文件却是好的**，所以只测文件发现不了这个缺陷。
mkdir -p "$CACHE/rootfs/ovdir" && echo x > "$CACHE/rootfs/ovdir/f"
run lbetest -- /bin/sh -c 'rm -rf /ovdir && echo RMDIR_OK'
if [ "$rc" -eq 0 ] && printf '%s' "$OUT" | grep -q RMDIR_OK && [ -f "$CACHE/rootfs/ovdir/f" ]; then
  report PASS "OV.6 容器内可删除镜像自带目录（userxattr 生效）且缓存不受影响"
else
  report FAIL "OV.6 删除镜像目录" "rc=$rc 输出=$(printf '%s' "$OUT"|head -c 80) 缓存仍在=$([ -f "$CACHE/rootfs/ovdir/f" ] && echo 是 || echo 否)"
fi
rm -rf "$CACHE/rootfs/ovdir"

echo
echo "=== PSH 镜像推送（PRD F9.13）==="

# 判据说明：**不打真 registry**。用 python3 起一个最小 registry stub（收
# POST/PUT、内存存 blob、PUT manifest 时自检引用的 blob 是否都已入库），
# 再用 wbox pull 从 stub 拉回来比对内容，形成 push→pull 闭环。
# 只断言"push 返回 0"是不够的——那证明不了推上去的东西能被拉回来用。
if ! command -v python3 >/dev/null 2>&1; then
  absent "PSH 镜像推送" "缺 python3（registry stub）"
else
PSH_PORT=5599
PSH_REG=127.0.0.1:$PSH_PORT
cat > "$WORK/registry_stub.py" <<'PSHEOF'
import http.server, hashlib, json
BLOBS = {}
MANIFESTS = {}

class H(http.server.BaseHTTPRequestHandler):
    def log_message(self, *a):
        pass

    def _empty(self, code, extra=None):
        self.send_response(code)
        for k, v in (extra or {}).items():
            self.send_header(k, v)
        self.send_header("Content-Length", "0")
        self.end_headers()

    def do_HEAD(self):
        d = self.path.rsplit("/", 1)[1]
        self._empty(200 if ("/blobs/" in self.path and d in BLOBS) else 404)

    def do_POST(self):
        self._empty(202, {"Location": "/v2/upload/session"})

    def do_PUT(self):
        n = int(self.headers.get("Content-Length", "0"))
        body = self.rfile.read(n)
        actual = "sha256:" + hashlib.sha256(body).hexdigest()
        if self.path.startswith("/v2/upload/"):
            want = self.path.split("digest=")[1].replace("%3A", ":") if "digest=" in self.path else actual
            if want != actual:
                self._empty(400)
                return
            BLOBS[actual] = body
            self._empty(201)
            return
        MANIFESTS[self.path] = body
        self._empty(201, {"Docker-Content-Digest": actual})
        m = json.loads(body)
        refs = [m["config"]["digest"]] + [l["digest"] for l in m["layers"]]
        missing = [d for d in refs if d not in BLOBS]
        print("MANIFEST_BLOBS_MISSING" if missing else "MANIFEST_BLOBS_PRESENT", flush=True)

    def do_GET(self):
        if self.path in MANIFESTS:
            b = MANIFESTS[self.path]
            self.send_response(200)
            self.send_header("Content-Type", "application/vnd.oci.image.manifest.v1+json")
            self.send_header("Content-Length", str(len(b)))
            self.end_headers()
            self.wfile.write(b)
            return
        d = self.path.rsplit("/", 1)[1]
        if "/blobs/" in self.path and d in BLOBS:
            b = BLOBS[d]
            self.send_response(200)
            self.send_header("Content-Length", str(len(b)))
            self.end_headers()
            self.wfile.write(b)
            return
        self._empty(404)

http.server.HTTPServer(("127.0.0.1", PORT_PLACEHOLDER), H).serve_forever()
PSHEOF
sed -i "s/PORT_PLACEHOLDER/$PSH_PORT/" "$WORK/registry_stub.py"
python3 "$WORK/registry_stub.py" > "$WORK/stub.log" 2>&1 &
PSH_PID=$!
sleep 1.5
# 确认 stub **自己**起来了。端口被别的进程占着时 bind 会失败，而用例照样能
# 连上"那一个"服务器并得到看似合理的结果——判据就此失去意义。实测遇到过：
# 一个遗留的手工 stub 占着端口，门禁的 stub 绑定失败，用例去跟它说话，
# 于是断言查不到本次运行的记录而报假失败。
if ! kill -0 "$PSH_PID" 2>/dev/null; then
  report FAIL "PSH registry stub" "stub 未能启动（端口 $PSH_PORT 被占用？）：$(head -c 200 "$WORK/stub.log")"
  PSH_PID=""
fi

# 造一个"已在本地"的镜像（含符号链接，用来验证打包不把它展开成副本）
PSHDIR=$WORK/home/.wbox/images/127.0.0.1_$PSH_PORT/library_pushed/v1
mkdir -p "$PSHDIR/rootfs/bin"
printf 'PUSHED_CONTENT\n' > "$PSHDIR/rootfs/greet.txt"
ln -sf greet.txt "$PSHDIR/rootfs/alias"
printf '{}\n' > "$PSHDIR/manifest.json"
printf '["sha256:l1"]\n' > "$PSHDIR/layers.json"
printf '{"config":{"Env":["PUSHED=yes"],"Cmd":["/bin/sh"]}}\n' > "$PSHDIR/config.json"

# PSH.1 不开逃生口时必须走 https —— 明文 registry 不能默认可用。
pout=$(HOME=$WORK/home "$WBOX_ABS" push "$PSH_REG/library/pushed:v1" 2>&1); prc=$?
if [ "$prc" -ne 0 ]; then
  report PASS "PSH.1 未设 WBOX_INSECURE_REGISTRY 时坚持 https（明文不默认可用）"
else
  report FAIL "PSH.1 默认 https" "明文 registry 竟然推成功了（rc=$prc）"
fi

# PSH.2 push 成功，且**stub 侧**确认 manifest 引用的 blob 都已先行入库。
# 顺序反了的话符合规范的 registry 会拒，这条盯的就是那个顺序。
pout=$(HOME=$WORK/home WBOX_INSECURE_REGISTRY=$PSH_REG "$WBOX_ABS" push "$PSH_REG/library/pushed:v1" 2>&1); prc=$?
sleep 0.5
if [ "$prc" -eq 0 ] && grep -q MANIFEST_BLOBS_PRESENT "$WORK/stub.log"; then
  report PASS "PSH.2 push 成功且 manifest 引用的 blob 已先行入库"
else
  report FAIL "PSH.2 push" "rc=$prc stub=$(head -c 80 "$WORK/stub.log") 输出=$(printf '%s' "$pout"|tail -c 120)"
fi

# PSH.3 平铺单层要说出来。推上去的与本地"看着一样"但分层没了，
# 不打印的话用户会以为原样回推。
if printf '%s' "$pout" | grep -q '单层'; then
  report PASS "PSH.3 push 明确告知平铺为单层（不让人误以为保留分层）"
else
  report FAIL "PSH.3 单层告知" "输出未提及: $(printf '%s' "$pout" | head -c 150)"
fi

# PSH.4 **闭环**：从 stub 拉回来，内容、符号链接、config 都要还原。
# 这条才是"推上去的东西真的能用"的证据。
PSHHOME=$WORK/pullback
mkdir -p "$PSHHOME"
pout=$(HOME=$PSHHOME WBOX_INSECURE_REGISTRY=$PSH_REG "$WBOX_ABS" pull "$PSH_REG/library/pushed:v1" 2>&1); prc=$?
PB=$PSHHOME/.wbox/images/127.0.0.1_$PSH_PORT/library_pushed/v1
if [ "$prc" -eq 0 ] && [ "$(cat "$PB/rootfs/greet.txt" 2>/dev/null)" = "PUSHED_CONTENT" ] \
   && [ -L "$PB/rootfs/alias" ] && grep -q 'PUSHED=yes' "$PB/config.json" 2>/dev/null; then
  report PASS "PSH.4 push→pull 闭环（内容/符号链接/config Env 均还原）"
else
  report FAIL "PSH.4 闭环" "rc=$prc 内容=$(cat "$PB/rootfs/greet.txt" 2>/dev/null|head -c 30) 软链=$([ -L "$PB/rootfs/alias" ] && echo 是 || echo 否)"
fi

# PSH.5 本地没有的镜像要明确报错并给出下一步，不能推一个空目录上去。
pout=$(HOME=$WORK/home WBOX_INSECURE_REGISTRY=$PSH_REG "$WBOX_ABS" push "$PSH_REG/library/absent:v9" 2>&1); prc=$?
if [ "$prc" -ne 0 ] && printf '%s' "$pout" | grep -q "本地没有镜像"; then
  report PASS "PSH.5 本地无此镜像时明确报错"
else
  report FAIL "PSH.5 缺镜像" "rc=$prc 输出: $(printf '%s' "$pout" | head -c 150)"
fi

kill "$PSH_PID" 2>/dev/null
wait "$PSH_PID" 2>/dev/null

# ---- PSH.6/PSH.7：多层镜像的**原样回推**（PRD L5 的判据）----
#
# 判据就是 PRD 写死的那条：多层镜像 pull 下来后能原样 push 回去，
# **manifest digest 不变**且层数不被压平。只断言"push 返回 0"证明不了这一点
# ——F9.13 的 flatten 路径同样返回 0，却会把多层压成一层。
PSH2_PORT=5601
PSH2_REG=127.0.0.1:$PSH2_PORT
cat > "$WORK/multi_stub.py" <<'PSH2EOF'
import http.server, hashlib, json, io, tarfile, gzip
BLOBS = {}
MANIFESTS = {}

def mklayer(files):
    buf = io.BytesIO()
    with tarfile.open(fileobj=buf, mode="w") as t:
        for name, data in files.items():
            ti = tarfile.TarInfo(name)
            ti.size = len(data)
            t.addfile(ti, io.BytesIO(data))
    return gzip.compress(buf.getvalue(), mtime=0)

def dg(b):
    return "sha256:" + hashlib.sha256(b).hexdigest()

L1 = mklayer({"base.txt": b"from-layer-1\n"})
L2 = mklayer({"extra.txt": b"from-layer-2\n"})
CFG = json.dumps({"architecture": "amd64", "os": "linux",
                  "config": {"Env": ["L=2"], "Cmd": ["/bin/sh"]},
                  "rootfs": {"type": "layers", "diff_ids": ["sha256:x", "sha256:y"]}},
                 separators=(",", ":")).encode()
for b in (L1, L2, CFG):
    BLOBS[dg(b)] = b
MT = "application/vnd.oci.image.manifest.v1+json"
MAN = json.dumps({"schemaVersion": 2, "mediaType": MT,
                  "config": {"mediaType": "application/vnd.oci.image.config.v1+json",
                             "digest": dg(CFG), "size": len(CFG)},
                  "layers": [{"mediaType": "application/vnd.oci.image.layer.v1.tar+gzip",
                              "digest": dg(L1), "size": len(L1)},
                             {"mediaType": "application/vnd.oci.image.layer.v1.tar+gzip",
                              "digest": dg(L2), "size": len(L2)}]},
                 separators=(",", ":")).encode()
MANIFESTS["/v2/library/multi/manifests/v1"] = MAN
print("ORIGINAL=" + dg(MAN), flush=True)

class H(http.server.BaseHTTPRequestHandler):
    def log_message(self, *a):
        pass

    def _e(self, c, x=None):
        self.send_response(c)
        for k, v in (x or {}).items():
            self.send_header(k, v)
        self.send_header("Content-Length", "0")
        self.end_headers()

    def do_HEAD(self):
        d = self.path.rsplit("/", 1)[1]
        self._e(200 if ("/blobs/" in self.path and d in BLOBS) else 404)

    def do_POST(self):
        self._e(202, {"Location": "/v2/upload/s"})

    def do_PUT(self):
        n = int(self.headers.get("Content-Length", "0"))
        body = self.rfile.read(n)
        a = dg(body)
        if self.path.startswith("/v2/upload/"):
            BLOBS[a] = body
            self._e(201)
            return
        MANIFESTS[self.path] = body
        self._e(201, {"Docker-Content-Digest": a})
        print("PUSHED=" + a, flush=True)
        print("LAYERS=%d" % len(json.loads(body)["layers"]), flush=True)

    def do_GET(self):
        if self.path in MANIFESTS:
            b = MANIFESTS[self.path]
            self.send_response(200)
            self.send_header("Content-Type", MT)
            self.send_header("Docker-Content-Digest", dg(b))
            self.send_header("Content-Length", str(len(b)))
            self.end_headers()
            self.wfile.write(b)
            return
        d = self.path.rsplit("/", 1)[1]
        if "/blobs/" in self.path and d in BLOBS:
            b = BLOBS[d]
            self.send_response(200)
            self.send_header("Content-Length", str(len(b)))
            self.end_headers()
            self.wfile.write(b)
            return
        self._e(404)

http.server.HTTPServer(("127.0.0.1", PORT2_PLACEHOLDER), H).serve_forever()
PSH2EOF
sed -i "s/PORT2_PLACEHOLDER/$PSH2_PORT/" "$WORK/multi_stub.py"
python3 "$WORK/multi_stub.py" > "$WORK/multi.log" 2>&1 &
PSH2_PID=$!
sleep 1.5
if ! kill -0 "$PSH2_PID" 2>/dev/null; then
  report FAIL "PSH 多层 registry stub" "stub 未能启动（端口 $PSH2_PORT 被占用？）：$(head -c 200 "$WORK/multi.log")"
  PSH2_PID=""
fi
PSH2HOME=$WORK/multihome
mkdir -p "$PSH2HOME"
pout=$(HOME=$PSH2HOME WBOX_INSECURE_REGISTRY=$PSH2_REG "$WBOX_ABS" pull "$PSH2_REG/library/multi:v1" 2>&1); prc=$?
MB=$PSH2HOME/.wbox/images/127.0.0.1_$PSH2_PORT/library_multi/v1
nblobs=$(ls "$MB/blobs" 2>/dev/null | wc -l)
if [ "$prc" -eq 0 ] && [ "$nblobs" -eq 2 ] \
   && [ "$(cat "$MB/rootfs/base.txt" 2>/dev/null)" = "from-layer-1" ] \
   && [ "$(cat "$MB/rootfs/extra.txt" 2>/dev/null)" = "from-layer-2" ]; then
  report PASS "PSH.6 pull 多层镜像后原始压缩层留存（$nblobs 层）且内容叠加正确"
else
  report FAIL "PSH.6 层留存" "rc=$prc blobs=$nblobs base=$(cat "$MB/rootfs/base.txt" 2>/dev/null)"
fi

pout=$(HOME=$PSH2HOME WBOX_INSECURE_REGISTRY=$PSH2_REG "$WBOX_ABS" push "$PSH2_REG/library/multi:v1" 2>&1); prc=$?
sleep 0.5
orig=$(grep '^ORIGINAL=' "$WORK/multi.log" | head -1 | cut -d= -f2)
pushed=$(grep '^PUSHED=' "$WORK/multi.log" | head -1 | cut -d= -f2)
nlayers=$(grep '^LAYERS=' "$WORK/multi.log" | head -1 | cut -d= -f2)
if [ "$prc" -eq 0 ] && [ -n "$orig" ] && [ "$orig" = "$pushed" ] && [ "$nlayers" = "2" ]; then
  report PASS "PSH.7 多层镜像原样回推：manifest digest 不变且未被压平（2 层）"
else
  report FAIL "PSH.7 原样回推" "rc=$prc 原始=$orig 推上去=$pushed 层数=$nlayers"
fi

# ---- PSH.8：构建产物的分层复用（PRD L5b 的判据）----
#
# 判据：以同一基础镜像构建的第二个镜像 push 时，**基础层被 registry 的 HEAD
# 判定已存在而跳过上传**，只实传增量。只看"push 成功"证明不了这一点——
# 压平成单层同样能推成功，但每次都要重传全部内容。
PSHCTX=$WORK/derivectx
mkdir -p "$PSHCTX"
printf 'DERIVED\n' > "$PSHCTX/added.txt"
printf 'FROM %s/library/multi:v1\nCOPY added.txt /added.txt\n' "$PSH2_REG" > "$PSHCTX/Dockerfile"
bout=$(HOME=$PSH2HOME WBOX_INSECURE_REGISTRY=$PSH2_REG "$WBOX_ABS" build \
  -t "$PSH2_REG/library/derived:v1" "$PSHCTX" 2>&1); brc=$?
DM=$PSH2HOME/.wbox/images/127.0.0.1_$PSH2_PORT/library_derived/v1/manifest.json
nl=$(python3 -c "import json,sys;print(len(json.load(open(sys.argv[1])).get('layers',[])))" "$DM" 2>/dev/null)
if [ "$brc" -eq 0 ] && [ "$nl" = "3" ]; then
  report PASS "PSH.8a 构建产物写成基础层+增量层（共 3 层，非压平）"
else
  report FAIL "PSH.8a 分层构建" "rc=$brc 层数=$nl（期望 3）"
fi

# 先把派生镜像推一次（此时基础层已在 registry，因为它们就是从这里拉的），
# 再看跳过计数。基础层必须被跳过，增量层必须实传。
pout=$(HOME=$PSH2HOME WBOX_INSECURE_REGISTRY=$PSH2_REG "$WBOX_ABS" push \
  "$PSH2_REG/library/derived:v1" 2>&1); prc=$?
if [ "$prc" -eq 0 ] && printf '%s' "$pout" | grep -q "层已在 registry 上，跳过上传"; then
  report PASS "PSH.8b push 派生镜像时基础层被跳过（分层复用生效）"
else
  report FAIL "PSH.8b 基础层复用" "rc=$prc 输出: $(printf '%s' "$pout" | tr '\n' ' ' | head -c 180)"
fi

# PSH.8c 闭环：派生镜像拉回来必须三层叠加正确——基础两层的内容加上增量层的。
# 分层推对了但内容错了，比不分层更糟。
PSH3HOME=$WORK/derivedhome
mkdir -p "$PSH3HOME"
HOME=$PSH3HOME WBOX_INSECURE_REGISTRY=$PSH2_REG "$WBOX_ABS" pull \
  "$PSH2_REG/library/derived:v1" >/dev/null 2>&1
DR=$PSH3HOME/.wbox/images/127.0.0.1_$PSH2_PORT/library_derived/v1/rootfs
if [ "$(cat "$DR/base.txt" 2>/dev/null)" = "from-layer-1" ] \
   && [ "$(cat "$DR/extra.txt" 2>/dev/null)" = "from-layer-2" ] \
   && [ "$(cat "$DR/added.txt" 2>/dev/null)" = "DERIVED" ]; then
  report PASS "PSH.8c 派生镜像拉回后三层叠加内容正确"
else
  report FAIL "PSH.8c 叠加内容" "base=$(cat "$DR/base.txt" 2>/dev/null) extra=$(cat "$DR/extra.txt" 2>/dev/null) added=$(cat "$DR/added.txt" 2>/dev/null)"
fi

kill "$PSH2_PID" 2>/dev/null
wait "$PSH2_PID" 2>/dev/null
fi

echo
echo "=== CMP compose 编排子集（PRD F9.14）==="

CMPDIR=$WORK/cproj
mkdir -p "$CMPDIR"
cat > "$CMPDIR/compose.yaml" <<'CMPEOF'
services:
  api:
    image: lbetest
    command: ["/bin/sleep", "60"]
    environment:
      - ROLE=api
  worker:
    image: lbetest
    depends_on:
      - api
    command: ["/bin/sleep", "60"]
CMPEOF

# CMP.1 up -d 起全部服务，容器名为 <项目>_<服务>。
cout=$(cd "$CMPDIR" && HOME=$WORK/home "$WBOX_ABS" compose up -d 2>&1); crc=$?
sleep 1
pslist=$(HOME=$WORK/home "$WBOX_ABS" ps 2>&1)
if [ "$crc" -eq 0 ] && printf '%s' "$pslist" | grep -q cproj_api && printf '%s' "$pslist" | grep -q cproj_worker; then
  report PASS "CMP.1 compose up -d 起全部服务（容器名 <项目>_<服务>）"
else
  report FAIL "CMP.1 up" "rc=$crc ps=$(printf '%s' "$pslist" | tr '\n' ' ' | head -c 160)"
fi

# CMP.2 **网络语义的行为判据**：服务之间共享同一 netns（第一个服务持有网络，
# 其余加入）。三方比对——只比两方会漏掉"两者都掉进宿主网络"。
# 这条同时是那个竞态的回归测试：加入方曾因 peer 的 container.pid 还没落盘
# 而直接退出，而 container.pid 又曾被 overlay 探测的短命子进程占掉。
nsA=$(timeout 15 env HOME=$WORK/home "$WBOX_ABS" exec cproj_api -- readlink /proc/self/ns/net 2>/dev/null)
nsB=$(timeout 15 env HOME=$WORK/home "$WBOX_ABS" exec cproj_worker -- readlink /proc/self/ns/net 2>/dev/null)
nsH=$(readlink /proc/self/ns/net)
if [ -n "$nsA" ] && [ "$nsA" = "$nsB" ] && [ "$nsA" != "$nsH" ]; then
  report PASS "CMP.2 服务间共享同一 netns（且非宿主网络）"
else
  report FAIL "CMP.2 共享网络" "api=$nsA worker=$nsB host=$nsH"
fi

# CMP.3 compose ps 反映真实状态。
cout=$(cd "$CMPDIR" && HOME=$WORK/home "$WBOX_ABS" compose ps 2>&1)
if printf '%s' "$cout" | grep -q 'api.*cproj_api.*running'; then
  report PASS "CMP.3 compose ps 显示服务与容器状态"
else
  report FAIL "CMP.3 ps" "输出: $(printf '%s' "$cout" | tr '\n' ' ' | head -c 160)"
fi

# CMP.4 down 清干净：容器记录不再存在。
cout=$(cd "$CMPDIR" && HOME=$WORK/home "$WBOX_ABS" compose down 2>&1); crc=$?
pslist=$(HOME=$WORK/home "$WBOX_ABS" ps -a 2>&1)
if [ "$crc" -eq 0 ] && ! printf '%s' "$pslist" | grep -q cproj_; then
  report PASS "CMP.4 compose down 停止并删除全部服务容器"
else
  report FAIL "CMP.4 down" "rc=$crc 残留: $(printf '%s' "$pslist" | tr '\n' ' ' | head -c 160)"
fi

# CMP.5 尚未支持的字段必须报错。静默忽略 build:/networks: 会让用户以为配置
# 生效了——对编排文件而言这是最危险的失败形态。
cat > "$CMPDIR/bad.yaml" <<'CMPEOF'
services:
  a:
    image: lbetest
    networks:
      - front
CMPEOF
cout=$(cd "$CMPDIR" && HOME=$WORK/home "$WBOX_ABS" compose -f bad.yaml up -d 2>&1); crc=$?
if [ "$crc" -ne 0 ] && printf '%s' "$cout" | grep -q "尚未支持的字段"; then
  report PASS "CMP.5 未支持字段明确报错（不静默忽略）"
else
  report FAIL "CMP.5 未支持字段" "rc=$crc 输出: $(printf '%s' "$cout" | head -c 160)"
fi

# CMP.6 循环依赖要点名报错，不能死循环或随便挑个顺序。
cat > "$CMPDIR/cycle.yaml" <<'CMPEOF'
services:
  a:
    image: lbetest
    depends_on:
      - b
  b:
    image: lbetest
    depends_on:
      - a
CMPEOF
cout=$(cd "$CMPDIR" && HOME=$WORK/home "$WBOX_ABS" compose -f cycle.yaml up -d 2>&1); crc=$?
if [ "$crc" -ne 0 ] && printf '%s' "$cout" | grep -q "循环依赖"; then
  report PASS "CMP.6 depends_on 循环依赖被拒绝并点名"
else
  report FAIL "CMP.6 循环依赖" "rc=$crc 输出: $(printf '%s' "$cout" | head -c 160)"
fi

# CMP.7 前台 up 要明说不支持，而不是偷偷加 -d（偷偷加会让用户以为命令挂了）。
cout=$(cd "$CMPDIR" && HOME=$WORK/home "$WBOX_ABS" compose up 2>&1); crc=$?
if [ "$crc" -ne 0 ] && printf '%s' "$cout" | grep -q "stdio"; then
  report PASS "CMP.7 compose up 缺 -d 时说明原因（不偷偷后台化）"
else
  report FAIL "CMP.7 缺 -d" "rc=$crc 输出: $(printf '%s' "$cout" | head -c 160)"
fi

echo
echo "=== IU IPC/UTS 隔离与共享（PRD F9.15）==="

# IU.1 IPC 与 UTS 默认**隔离**。此前 wbox 让容器直接共享宿主的这两个
# namespace——guest 能碰宿主的 System V 对象、能 sethostname 改掉宿主主机名。
# 这是真实的隔离缺口，不是"少一个特性"，故判据是与宿主 inode **不同**。
run lbetest -- /bin/sh -c 'readlink /proc/self/ns/ipc; readlink /proc/self/ns/uts'
cipc=$(printf '%s' "$OUT" | sed -n 1p)
cuts=$(printf '%s' "$OUT" | sed -n 2p)
hipc=$(readlink /proc/self/ns/ipc)
huts=$(readlink /proc/self/ns/uts)
if [ "$rc" -eq 0 ] && [ -n "$cipc" ] && [ "$cipc" != "$hipc" ] && [ "$cuts" != "$huts" ]; then
  report PASS "IU.1 IPC/UTS 默认与宿主隔离"
else
  report FAIL "IU.1 默认隔离" "容器 ipc=$cipc uts=$cuts；宿主 ipc=$hipc uts=$huts"
fi

# IU.2 主机名默认取容器名（与 docker 一致），且**宿主主机名不受影响**。
# 后半条是关键：UTS 没隔离时 sethostname 会改掉宿主的。
hostbefore=$(hostname)
run --name iuhost lbetest -- /bin/hostname
if [ "$rc" -eq 0 ] && [ "$(printf '%s' "$OUT")" = "iuhost" ] && [ "$(hostname)" = "$hostbefore" ]; then
  report PASS "IU.2 主机名默认取容器名，且不改动宿主主机名"
else
  report FAIL "IU.2 主机名" "容器内=$(printf '%s' "$OUT"|head -c 40) 宿主 before=$hostbefore after=$(hostname)"
fi

# IU.3 --hostname 覆盖默认值。
run --name iuh2 --hostname custom.local lbetest -- /bin/hostname
if [ "$rc" -eq 0 ] && [ "$(printf '%s' "$OUT")" = "custom.local" ]; then
  report PASS "IU.3 --hostname 覆盖默认容器名"
else
  report FAIL "IU.3 --hostname" "输出: $(printf '%s' "$OUT" | head -c 60)"
fi

# IU.4/IU.5 共享：--ipc/--uts container: 加入既有容器的 namespace。
# 判据取自**容器内**（经 exec），不是宿主视角——exec 现在也会进 ipc/uts，
# 早先它只进 user/mnt/net/pid，量到的是宿主的，那次差点误判成"共享没生效"。
HOME=$WORK/home "$WBOX_ABS" run -d --name iupeer lbetest -- /bin/sleep 30 >/dev/null 2>&1
sleep 1
pipc=$(timeout 15 env HOME=$WORK/home "$WBOX_ABS" exec iupeer -- readlink /proc/self/ns/ipc 2>/dev/null)
puts=$(timeout 15 env HOME=$WORK/home "$WBOX_ABS" exec iupeer -- readlink /proc/self/ns/uts 2>/dev/null)
jipc=$(timeout 20 env HOME=$WORK/home "$WBOX_ABS" run --rm --name iuj1 --ipc container:iupeer lbetest -- readlink /proc/self/ns/ipc 2>/dev/null)
juts=$(timeout 20 env HOME=$WORK/home "$WBOX_ABS" run --rm --name iuj2 --uts container:iupeer lbetest -- readlink /proc/self/ns/uts 2>/dev/null)
if [ -n "$pipc" ] && [ "$pipc" = "$jipc" ] && [ "$pipc" != "$hipc" ]; then
  report PASS "IU.4 --ipc container: 共享目标容器的 IPC namespace"
else
  report FAIL "IU.4 --ipc container:" "peer=$pipc joiner=$jipc host=$hipc"
fi
if [ -n "$puts" ] && [ "$puts" = "$juts" ] && [ "$puts" != "$huts" ]; then
  report PASS "IU.5 --uts container: 共享目标容器的 UTS namespace"
else
  report FAIL "IU.5 --uts container:" "peer=$puts joiner=$juts host=$huts"
fi

# IU.6 exec 必须落在容器的 UTS 里。早先它不进 ipc/uts，于是 exec 看到的是
# 宿主主机名——用户以为自己在容器内，看到的却是宿主。
eout=$(timeout 15 env HOME=$WORK/home "$WBOX_ABS" exec iupeer -- hostname 2>/dev/null)
if [ "$eout" = "iupeer" ]; then
  report PASS "IU.6 exec 落在容器的 UTS namespace（看到容器主机名）"
else
  report FAIL "IU.6 exec UTS" "exec 看到的主机名=$eout（期望 iupeer）"
fi

# IU.7 三个 container: 指向不同容器要报错：加入模式先进目标 userns，
# 而一个进程只能在一个 userns 里。挑一个赢家等于悄悄忽略另一个。
iout=$(HOME=$WORK/home "$WBOX_ABS" run --ipc container:iupeer --uts container:other lbetest -- /bin/true 2>&1); irc=$?
if [ "$irc" -ne 0 ] && printf '%s' "$iout" | grep -q "不同的容器"; then
  report PASS "IU.7 --network/--ipc/--uts 指向不同容器时报错"
else
  report FAIL "IU.7 多目标冲突" "rc=$irc 输出: $(printf '%s' "$iout" | head -c 150)"
fi

HOME=$WORK/home "$WBOX_ABS" kill iupeer >/dev/null 2>&1
HOME=$WORK/home "$WBOX_ABS" rm iupeer iuhost iuh2 >/dev/null 2>&1

echo
echo "=== DF wbox diff（PRD F9.19）==="

# 三类改动一次测全：A 新增、C 改动、D 删除。只测其中一类会漏掉分类逻辑的错——
# 而分类错的表现是"清单看着有内容"，很容易蒙混过去。
mkdir -p "$CACHE/rootfs/dfetc"
printf 'orig\n' > "$CACHE/rootfs/dfetc/conf"
printf 'gone\n' > "$CACHE/rootfs/dfetc/todelete"
HOME=$WORK/home "$WBOX_ABS" run -d --name dfc lbetest -- \
  /bin/sh -c 'echo new > /dfetc/conf; rm /dfetc/todelete; echo x > /dfnew.txt; sleep 20' >/dev/null 2>&1
sleep 2
dout=$(HOME=$WORK/home "$WBOX_ABS" diff dfc 2>&1); drc=$?
if [ "$drc" -eq 0 ] \
   && printf '%s' "$dout" | grep -qx 'A /dfnew.txt' \
   && printf '%s' "$dout" | grep -qx 'C /dfetc/conf' \
   && printf '%s' "$dout" | grep -qx 'D /dfetc/todelete'; then
  report PASS "DF.1 diff 正确区分新增/改动/删除（A/C/D，与 docker 对齐）"
else
  report FAIL "DF.1 三类改动" "rc=$drc 输出: $(printf '%s' "$dout" | tr '\n' ' ' | head -c 200)"
fi

# wbox 自己的换根暂存目录不该出现在用户看的清单里——那是实现细节，
# 混进答案里等于让用户替我们记住内部约定。
if ! printf '%s' "$dout" | grep -q '.wbox_oldroot'; then
  report PASS "DF.2 内部产物 .wbox_oldroot 不出现在 diff 输出中"
else
  report FAIL "DF.2 内部产物泄漏" "输出: $(printf '%s' "$dout" | tr '\n' ' ' | head -c 160)"
fi
HOME=$WORK/home "$WBOX_ABS" kill dfc >/dev/null 2>&1
HOME=$WORK/home "$WBOX_ABS" rm dfc >/dev/null 2>&1

# 宿主程序模式没有 overlay 层，拿不出这个答案：必须报错而不是打印空清单
# ——空清单会被读成"没改过"，那是把"不知道"说成了"没有"。
HOME=$WORK/home "$WBOX_ABS" run -d --name dfhost -- /bin/sleep 10 >/dev/null 2>&1
sleep 1
dout=$(HOME=$WORK/home "$WBOX_ABS" diff dfhost 2>&1); drc=$?
if [ "$drc" -ne 0 ] && printf '%s' "$dout" | grep -q "宿主程序模式"; then
  report PASS "DF.3 无 overlay 层时报错并说明原因（不打印空清单）"
else
  report FAIL "DF.3 无 overlay 层" "rc=$drc 输出: $(printf '%s' "$dout" | head -c 160)"
fi
HOME=$WORK/home "$WBOX_ABS" kill dfhost >/dev/null 2>&1
HOME=$WORK/home "$WBOX_ABS" rm dfhost >/dev/null 2>&1
rm -rf "$CACHE/rootfs/dfetc"

echo
echo "=== CM wbox commit（PRD F9.20）==="

# 判据分两半，缺一不可：改动真的固化进新镜像；基础镜像**未被动过**。
# 后半条是这条链路（硬链接 + overlay 合并）最危险的失败形态。
mkdir -p "$CACHE/rootfs/cmetc"
printf 'orig\n' > "$CACHE/rootfs/cmetc/conf"
printf 'gone\n' > "$CACHE/rootfs/cmetc/todelete"
cmbase=$(find "$CACHE/rootfs" -type f -exec sha256sum {} \; | sort | sha256sum)
HOME=$WORK/home "$WBOX_ABS" run -d --name cmc lbetest -- \
  /bin/sh -c 'echo new > /cmetc/conf; rm /cmetc/todelete; echo x > /cmadded.txt; sleep 30' >/dev/null 2>&1
sleep 2
cout=$(HOME=$WORK/home "$WBOX_ABS" commit cmc cmmine:v1 2>&1); crc=$?

# 新镜像跑起来：新增在、改动生效、删除也固化了
vout=$(HOME=$WORK/home "$WBOX_ABS" run --rm --name cmv cmmine:v1 -- \
  /bin/sh -c 'cat /cmetc/conf; cat /cmadded.txt; test -e /cmetc/todelete && echo STILL || echo DELETED' 2>&1)
if [ "$crc" -eq 0 ] \
   && printf '%s' "$vout" | grep -qx 'new' \
   && printf '%s' "$vout" | grep -qx 'x' \
   && printf '%s' "$vout" | grep -qx 'DELETED'; then
  report PASS "CM.1 commit 固化新增/改动/删除三类改动"
else
  report FAIL "CM.1 改动固化" "rc=$crc 输出: $(printf '%s' "$vout" | tr '\n' ' ' | head -c 180)"
fi

cmafter=$(find "$CACHE/rootfs" -type f -exec sha256sum {} \; | sort | sha256sum)
if [ "$cmbase" = "$cmafter" ] && [ "$(cat "$CACHE/rootfs/cmetc/conf")" = "orig" ] \
   && [ -f "$CACHE/rootfs/cmetc/todelete" ]; then
  report PASS "CM.2 commit 后基础镜像缓存逐字节不变"
else
  report FAIL "CM.2 基础镜像完整性" "摘要变了=$([ "$cmbase" = "$cmafter" ] && echo 否 || echo 是) conf=$(cat "$CACHE/rootfs/cmetc/conf" 2>/dev/null)"
fi

# 磁盘共享：commit 出来的镜像与基础镜像共享数据块
CMD1=$WORK/home/.wbox/images/registry-1.docker.io/library_cmmine/v1/rootfs
cb1=$(du -s "$CACHE/rootfs" | cut -f1)
cb2=$(du -s "$CMD1" | cut -f1)
cboth=$(du -sc "$CACHE/rootfs" "$CMD1" | tail -1 | cut -f1)
if [ "$cboth" -lt $(( (cb1 + cb2) * 3 / 4 )) ]; then
  report PASS "CM.3 commit 产物与基础镜像共享磁盘（合计 ${cboth}KB，两份之和 $((cb1+cb2))KB）"
else
  report FAIL "CM.3 磁盘共享" "合计=${cboth}KB 两份之和=$((cb1+cb2))KB"
fi

# 覆盖基础镜像会在铺硬链接中途把 lower 抽掉，必须拒绝
cout=$(HOME=$WORK/home "$WBOX_ABS" commit cmc lbetest:latest 2>&1); crc=$?
if [ "$crc" -ne 0 ] && printf '%s' "$cout" | grep -q "拒绝原地覆盖"; then
  report PASS "CM.4 拒绝 commit 覆盖容器自己的基础镜像"
else
  report FAIL "CM.4 原地覆盖守卫" "rc=$crc 输出: $(printf '%s' "$cout" | head -c 160)"
fi

HOME=$WORK/home "$WBOX_ABS" kill cmc >/dev/null 2>&1
HOME=$WORK/home "$WBOX_ABS" rm cmc >/dev/null 2>&1
HOME=$WORK/home "$WBOX_ABS" rmi cmmine:v1 >/dev/null 2>&1
rm -rf "$CACHE/rootfs/cmetc"

echo
echo "=== PZ pause / unpause（PRD F9.21）==="

# **行为判据**：让容器不停往宿主可见的文件里写计数，pause 后计数必须不再增长。
# 只断言"pause 返回 0"证明不了任何事——信号发出去了不等于进程真停了。
PZFILE=$WORK/pzcount
rm -f "$PZFILE"
HOME=$WORK/home "$WBOX_ABS" run -d --name pzc -v "$WORK:/pz" lbetest -- \
  /bin/sh -c 'i=0; while :; do i=$((i+1)); echo $i > /pz/pzcount; sleep 0.05; done' >/dev/null 2>&1
sleep 2
pout=$(HOME=$WORK/home "$WBOX_ABS" pause pzc 2>&1); prc=$?
# 两次采样都在 pause **之后**。先前拿 pause 之前的读数当基线是错的：
# 从采样到 pause 真正生效之间有几十毫秒，容器照跑，计数会 +1，
# 于是判据偶发红而产品无恙（实测撞到过一次）。要断言的本来就是
# "停下来之后不再变"，那就该只看停下来之后的两个点。
sleep 0.3
during=$(cat "$PZFILE" 2>/dev/null)
sleep 1
still=$(cat "$PZFILE" 2>/dev/null)
if [ "$prc" -eq 0 ] && [ -n "$during" ] && [ "$during" = "$still" ]; then
  report PASS "PZ.1 pause 后容器真的停止工作（计数冻结在 $during）"
else
  report FAIL "PZ.1 pause 冻结" "rc=$prc 停后立即=$during 再过 1 秒=$still（应相同）"
fi

pout=$(HOME=$WORK/home "$WBOX_ABS" unpause pzc 2>&1); prc=$?
sleep 1
after=$(cat "$PZFILE" 2>/dev/null)
if [ "$prc" -eq 0 ] && [ -n "$after" ] && [ "$after" != "$still" ]; then
  report PASS "PZ.2 unpause 后容器恢复工作（计数 $still → $after）"
else
  report FAIL "PZ.2 unpause 恢复" "rc=$prc 暂停时=$still 恢复 1 秒后=$after（应不同）"
fi
HOME=$WORK/home "$WBOX_ABS" kill pzc >/dev/null 2>&1
HOME=$WORK/home "$WBOX_ABS" rm pzc >/dev/null 2>&1
rm -f "$PZFILE"

# 已退出的容器要报错。静默成功会让脚本以为它还在、还能 unpause 回来。
pout=$(HOME=$WORK/home "$WBOX_ABS" pause nosuchpz 2>&1); prc=$?
if [ "$prc" -ne 0 ]; then
  report PASS "PZ.3 pause 不存在的容器时报错"
else
  report FAIL "PZ.3 未知容器" "rc=$prc 输出: $(printf '%s' "$pout" | head -c 120)"
fi

echo
echo "=== CP wbox cp（PRD F9.23）==="

# 这一组要钉死三件事，每一件都对应一种"看着成功其实错了"的失败：
#   1. 拷出来的是**容器现状**（upper 优先），不是镜像里那份旧的；
#   2. 拷进去的对**运行中的容器立即可见**（overlay 是活的，不是快照）；
#   3. 容器删过的文件**不会从镜像下层复活**（whiteout 要认）。
mkdir -p "$CACHE/rootfs/cpetc"
printf 'from-image\n' > "$CACHE/rootfs/cpetc/conf"
printf 'from-image\n' > "$CACHE/rootfs/cpetc/doomed"
cpbase=$(find "$CACHE/rootfs" -type f -exec sha256sum {} \; | sort | sha256sum)

HOME=$WORK/home "$WBOX_ABS" run -d --name cpc lbetest -- \
  /bin/sh -c 'echo from-container > /cpetc/conf; rm /cpetc/doomed; sleep 30' >/dev/null 2>&1
sleep 2

# CP.1 容器 → 宿主：必须拿到容器改过之后的内容
rm -f "$WORK/out.conf"
cout=$(HOME=$WORK/home "$WBOX_ABS" cp cpc:/cpetc/conf "$WORK/out.conf" 2>&1); crc=$?
if [ "$crc" -eq 0 ] && [ "$(cat "$WORK/out.conf" 2>/dev/null)" = "from-container" ]; then
  report PASS "CP.1 cp 出来的是容器现状而非镜像原件"
else
  report FAIL "CP.1 容器→宿主" "rc=$crc 内容=$(cat "$WORK/out.conf" 2>/dev/null | head -c 60)"
fi

# CP.2 容器删掉的文件不能从镜像下层复活。**必须真的失败**：
# 若这里静默成功，用户会拿着一份早已不存在的文件当成容器现状。
rm -f "$WORK/out.doomed"
cout=$(HOME=$WORK/home "$WBOX_ABS" cp cpc:/cpetc/doomed "$WORK/out.doomed" 2>&1); crc=$?
if [ "$crc" -ne 0 ] && [ ! -e "$WORK/out.doomed" ]; then
  report PASS "CP.2 容器删过的文件不从镜像下层复活"
else
  report FAIL "CP.2 whiteout 识别" "rc=$crc 落盘=$([ -e "$WORK/out.doomed" ] && echo 是 || echo 否) 输出: $(printf '%s' "$cout" | head -c 120)"
fi

# CP.3 宿主 → 容器：对**运行中的**容器立即可见（判据是容器自己读到的内容）
printf 'injected\n' > "$WORK/in.txt"
cout=$(HOME=$WORK/home "$WBOX_ABS" cp "$WORK/in.txt" cpc:/cpetc/in.txt 2>&1); crc=$?
seen=$(HOME=$WORK/home "$WBOX_ABS" exec cpc -- /bin/cat /cpetc/in.txt 2>&1)
if [ "$crc" -eq 0 ] && printf '%s' "$seen" | grep -qx 'injected'; then
  report PASS "CP.3 拷进去的文件对运行中的容器立即可见"
else
  report FAIL "CP.3 宿主→容器" "rc=$crc 容器内读到: $(printf '%s' "$seen" | tr '\n' ' ' | head -c 120)"
fi

# CP.4 写只写 upper：基础镜像缓存必须逐字节不变。
# 镜像目录是多个容器共享的（F9.18 硬链接），写穿了会污染别人。
cpafter=$(find "$CACHE/rootfs" -type f -exec sha256sum {} \; | sort | sha256sum)
if [ "$cpbase" = "$cpafter" ]; then
  report PASS "CP.4 cp 写入不触碰共享的基础镜像缓存"
else
  report FAIL "CP.4 镜像完整性" "cp 前后镜像摘要不一致"
fi

# CP.5 目录整棵拷进去
mkdir -p "$WORK/tree/sub"
printf 'deep\n' > "$WORK/tree/sub/leaf"
cout=$(HOME=$WORK/home "$WBOX_ABS" cp "$WORK/tree" cpc:/cptree 2>&1); crc=$?
seen=$(HOME=$WORK/home "$WBOX_ABS" exec cpc -- /bin/cat /cptree/sub/leaf 2>&1)
if [ "$crc" -eq 0 ] && printf '%s' "$seen" | grep -qx 'deep'; then
  report PASS "CP.5 目录递归拷入并保持层级"
else
  report FAIL "CP.5 目录拷贝" "rc=$crc 容器内读到: $(printf '%s' "$seen" | tr '\n' ' ' | head -c 120)"
fi

# CP.6 两端都不是容器路径时要报错。含冒号的宿主文件名（./a:b）不能被误判成容器。
cout=$(HOME=$WORK/home "$WBOX_ABS" cp "$WORK/in.txt" "$WORK/plain.txt" 2>&1); crc=$?
if [ "$crc" -ne 0 ] && printf '%s' "$cout" | grep -q '容器'; then
  report PASS "CP.6 两端都不是容器路径时报错并说清用法"
else
  report FAIL "CP.6 参数形态校验" "rc=$crc 输出: $(printf '%s' "$cout" | head -c 120)"
fi

HOME=$WORK/home "$WBOX_ABS" kill cpc >/dev/null 2>&1
HOME=$WORK/home "$WBOX_ABS" rm cpc >/dev/null 2>&1
rm -rf "$CACHE/rootfs/cpetc" "$WORK/tree" "$WORK/in.txt" "$WORK/out.conf"

echo
echo "=== ST wbox stats（PRD F9.24）==="

# 判据是**区分能力**，不是"命令跑通了"。一个死循环烧 CPU、一个纯 sleep，
# stats 必须把这两个区分开——只断言 rc=0 的话，一个恒返回 0.00% 的实现也能过。
HOME=$WORK/home "$WBOX_ABS" run -d --name stbusy lbetest -- \
  /bin/sh -c 'while :; do :; done' >/dev/null 2>&1
HOME=$WORK/home "$WBOX_ABS" run -d --name stidle lbetest -- /bin/sleep 30 >/dev/null 2>&1
sleep 2
stout=$(HOME=$WORK/home "$WBOX_ABS" stats stbusy stidle 2>&1); strc=$?

stb=$(printf '%s\n' "$stout" | awk '$1=="stbusy"{print $2}' | tr -d '%')
sti=$(printf '%s\n' "$stout" | awk '$1=="stidle"{print $2}' | tr -d '%')
# busy 至少要有明显占用；idle 必须接近 0。阈值取得宽松，判的是"分得开"。
if [ "$strc" -eq 0 ] \
   && [ -n "$stb" ] && [ -n "$sti" ] \
   && awk "BEGIN{exit !($stb > 20)}" \
   && awk "BEGIN{exit !($sti < 5)}"; then
  report PASS "ST.1 stats 区分得开忙闲容器（busy ${stb}% vs idle ${sti}%）"
else
  report FAIL "ST.1 CPU% 区分度" "rc=$strc busy=$stb idle=$sti 输出: $(printf '%s' "$stout" | tr '\n' '|' | head -c 200)"
fi

# 内存与进程数要是真数字。0 内存意味着采样根本没落到容器上。
stmem=$(printf '%s\n' "$stout" | awk '$1=="stidle"{print $3}')
stpid=$(printf '%s\n' "$stout" | awk '$1=="stidle"{print $4}')
if printf '%s' "$stmem" | grep -qE '^[0-9.]+(B|KiB|MiB|GiB|TiB)$' \
   && [ -n "$stpid" ] && [ "$stpid" -ge 1 ] 2>/dev/null; then
  report PASS "ST.2 内存与进程数取到真实数值（$stmem / $stpid 个进程）"
else
  report FAIL "ST.2 内存与进程数" "内存=$stmem 进程=$stpid"
fi

# 两种数据来源精度不同，必须标注；落到 /proc 那条路时还要打出重复计入的脚注。
# 印成一个样子会让人把 RSS 合计当成 cgroup 那种精确值。
stsrc=$(printf '%s\n' "$stout" | awk '$1=="stidle"{print $5}')
if [ "$stsrc" = "cgroup" ]; then
  report PASS "ST.3 标注数据来源（本机走 cgroup 精确记账）"
elif [ "$stsrc" = "proc*" ] && printf '%s' "$stout" | grep -q '重复计入'; then
  report PASS "ST.3 标注数据来源，并说明 RSS 合计会重复计入共享页"
else
  report FAIL "ST.3 来源标注" "来源列=$stsrc 有脚注=$(printf '%s' "$stout" | grep -qc 重复计入 && echo 是 || echo 否)"
fi

# 不带名字时列出全部运行中的容器（与 ps 默认视图一致）
stout=$(HOME=$WORK/home "$WBOX_ABS" stats 2>&1); strc=$?
if [ "$strc" -eq 0 ] && printf '%s' "$stout" | grep -q stbusy && printf '%s' "$stout" | grep -q stidle; then
  report PASS "ST.4 不带名字时列出全部运行中的容器"
else
  report FAIL "ST.4 全量列出" "rc=$strc 输出: $(printf '%s' "$stout" | tr '\n' '|' | head -c 200)"
fi

HOME=$WORK/home "$WBOX_ABS" kill stbusy >/dev/null 2>&1
HOME=$WORK/home "$WBOX_ABS" kill stidle >/dev/null 2>&1
sleep 1

# 已退出的容器要报错。打印一行 0 会被读成"这个容器很闲"，而不是"它已经没了"。
stout=$(HOME=$WORK/home "$WBOX_ABS" stats stidle 2>&1); strc=$?
if [ "$strc" -ne 0 ] && printf '%s' "$stout" | grep -q '已退出'; then
  report PASS "ST.5 已退出的容器报错而非打印一行 0"
else
  report FAIL "ST.5 已退出容器" "rc=$strc 输出: $(printf '%s' "$stout" | head -c 150)"
fi

HOME=$WORK/home "$WBOX_ABS" rm stbusy >/dev/null 2>&1
HOME=$WORK/home "$WBOX_ABS" rm stidle >/dev/null 2>&1

echo
echo "=== RMF ps -q / rm -f（PRD F9.29）==="

# 这一组只碰状态记录，不需要镜像；用独立 HOME + 宿主程序模式，与别的组无关。
RFH=$WORK/rfhome
rm -rf "$RFH" && mkdir -p "$RFH"
HOME=$RFH "$WBOX_ABS" run -d --name rf1 -- /bin/sleep 30 >/dev/null 2>&1
HOME=$RFH "$WBOX_ABS" run -d --name rf2 -- /bin/echo done >/dev/null 2>&1
sleep 2

# -q 是给脚本用的：**只能出名字**。空表说明、表头混进去都会被当成容器名传下去。
fout=$(HOME=$RFH "$WBOX_ABS" ps -aq 2>&1); frc=$?
fbad=$(printf '%s\n' "$fout" | grep -vcE '^(rf1|rf2)$')
if [ "$frc" -eq 0 ] && [ "$(printf '%s\n' "$fout" | wc -l)" -eq 2 ] && [ "$fbad" -eq 0 ]; then
  report PASS "RMF.1 ps -aq 只出名字（无表头、无说明行）"
else
  report FAIL "RMF.1 ps -aq" "rc=$frc 输出: $(printf '%s' "$fout" | tr '\n' '|' | head -c 150)"
fi

# 默认视图（不带 -a）的 -q 只列运行中的
fout=$(HOME=$RFH "$WBOX_ABS" ps -q 2>&1)
if [ "$(printf '%s\n' "$fout" | grep -c .)" -eq 1 ] && printf '%s' "$fout" | grep -qx rf1; then
  report PASS "RMF.2 ps -q 只列运行中的容器"
else
  report FAIL "RMF.2 ps -q" "输出: $(printf '%s' "$fout" | tr '\n' '|' | head -c 120)"
fi

# 不带 -f 时必须拒绝运行中的容器，且**记录还在**——这是 rm 的核心约定
fout=$(HOME=$RFH "$WBOX_ABS" rm rf1 2>&1); frc=$?
if [ "$frc" -ne 0 ] && HOME=$RFH "$WBOX_ABS" ps | awk '$1=="rf1"{f=1} END{exit !f}'; then
  report PASS "RMF.3 不带 -f 时拒绝删运行中的容器（记录与进程都还在）"
else
  report FAIL "RMF.3 默认拒绝" "rc=$frc 输出: $(printf '%s' "$fout" | head -c 150)"
fi

# -f 要**先停再删**：判据不只是记录没了，还要求那棵进程树真的没了——
# 只删记录不停进程会留下没人管得到的孤儿，而 ps 从此看不见它。
# 判活**不能用 `kill -0`**：僵尸进程（已死、尚未被回收）照样返回成功。
# 本机 PID 1 不回收孤儿，supervisor 被 setsid 脱离后死掉就会停在 Z 状态，
# 于是 `kill -0` 会把一个已经死了的进程报成活着——第一版判据就是这么假红的。
# 改看 /proc/<pid>/stat 的状态字段：Z 视为已死，取不到（进程没了）同样视为已死。
alive() {
  [ -r "/proc/$1/stat" ] || return 1
  # comm 字段可能含空格和括号，从最后一个 ')' 之后切
  st=$(sed -e 's/.*) //' "/proc/$1/stat" 2>/dev/null | cut -d' ' -f1)
  [ -n "$st" ] && [ "$st" != "Z" ]
}
rfpid=$(HOME=$RFH "$WBOX_ABS" ps | awk '$1=="rf1"{print $3}')
fout=$(HOME=$RFH "$WBOX_ABS" rm -f rf1 2>&1); frc=$?
sleep 1
if [ "$frc" -eq 0 ] \
   && ! HOME=$RFH "$WBOX_ABS" ps -a | awk '$1=="rf1"{f=1} END{exit !f}' \
   && ! alive "$rfpid"; then
  report PASS "RMF.4 rm -f 先停再删（supervisor pid $rfpid 已终止，记录也没了）"
else
  report FAIL "RMF.4 rm -f" "rc=$frc pid $rfpid 仍在运行=$(alive "$rfpid" && echo 是 || echo 否) 输出: $(printf '%s' "$fout" | head -c 120)"
fi

# 组合起来就是 docker 用户的清场惯用法
HOME=$RFH "$WBOX_ABS" run -d --name rf3 -- /bin/sleep 30 >/dev/null 2>&1
sleep 1
HOME=$RFH "$WBOX_ABS" rm -f $(HOME=$RFH "$WBOX_ABS" ps -aq) >/dev/null 2>&1
fout=$(HOME=$RFH "$WBOX_ABS" ps -aq 2>&1)
if [ -z "$(printf '%s' "$fout" | tr -d '[:space:]')" ]; then
  report PASS "RMF.5 wbox rm -f \$(wbox ps -aq) 清场干净"
else
  report FAIL "RMF.5 清场惯用法" "残留: $(printf '%s' "$fout" | tr '\n' '|' | head -c 120)"
fi

# stop / pause 也要收多个名字：`kill`/`rm`/`start` 早就收多个，唯独它俩只收一个
# 是纯粹的不一致——同一批容器能一起 kill 却不能一起 stop，用户没法从别的命令
# 推断出这条例外（F9.30）。
for n in mn1 mn2 mn3; do
  HOME=$RFH "$WBOX_ABS" run -d --name $n -- /bin/sleep 30 >/dev/null 2>&1
done
sleep 2
fout=$(HOME=$RFH "$WBOX_ABS" stop mn1 mn2 mn3 2>&1); frc=$?
sleep 1
fleft=$(HOME=$RFH "$WBOX_ABS" ps -q 2>&1 | grep -c '^mn')
if [ "$frc" -eq 0 ] && [ "$fleft" -eq 0 ]; then
  report PASS "RMF.6 stop 一次收多个容器名（三个全停下）"
else
  report FAIL "RMF.6 stop 多名" "rc=$frc 仍在运行=$fleft 输出: $(printf '%s' "$fout" | tr '\n' '|' | head -c 150)"
fi

# 单个容器失败时，主错误要给**真正的原因**，而不是"1 个容器未成功"这种汇总
fout=$(HOME=$RFH "$WBOX_ABS" pause mn1 2>&1); frc=$?
if [ "$frc" -ne 0 ] && printf '%s' "$fout" | grep -q '已退出' \
   && ! printf '%s' "$fout" | grep -q '共 1 个'; then
  report PASS "RMF.7 单个容器失败时直接给原因（不套无信息量的汇总）"
else
  report FAIL "RMF.7 单容器错误信息" "rc=$frc 输出: $(printf '%s' "$fout" | tr '\n' '|' | head -c 150)"
fi

HOME=$RFH "$WBOX_ABS" rm -f $(HOME=$RFH "$WBOX_ABS" ps -aq) >/dev/null 2>&1
rm -rf "$RFH"

echo
echo "=== LG wbox logs -f / --tail（PRD F9.28）==="

# 这一组不需要镜像（只读状态目录里的日志文件），用独立 HOME + 宿主程序模式，
# 与别的组彻底不相干。
LGH=$WORK/lghome
rm -rf "$LGH" && mkdir -p "$LGH"
HOME=$LGH "$WBOX_ABS" run -d --name lgc -- \
  /bin/sh -c 'for i in 1 2 3 4 5 6 7 8; do echo line$i; sleep 0.7; done' >/dev/null 2>&1

# --tail：只要最后几行，且不能把结尾的换行当成一整行（那会让 --tail 1 输出空行）
sleep 2
lout=$(HOME=$LGH "$WBOX_ABS" logs --tail 2 lgc 2>&1)
lcnt=$(printf '%s\n' "$lout" | grep -c '^line')
if [ "$lcnt" -eq 2 ] && printf '%s' "$lout" | grep -q 'line'; then
  report PASS "LG.1 --tail N 只输出最后 N 行（实得 $lcnt 行）"
else
  report FAIL "LG.1 --tail" "行数=$lcnt 输出: $(printf '%s' "$lout" | tr '\n' '|' | head -c 120)"
fi

# --follow 的判据是**它真的等到了后来才产生的输出**。
# 跟随必须在容器退出后自行结束（否则这里会一直挂到 timeout）——
# 用 timeout 兜底，但正常路径不该触发它。
# 容器整段输出跨约 5.6 秒，跟随从第 2 秒起接手，故必然要等 3 秒以上——
# 判据取 >=1 秒是留足余量，不是卡在临界点上。
# 还要证明它**确实等了**：只数行数的话，若容器早已跑完，一次性读全也能凑够
# 8 行——那证明不了跟随。所以同时要求这条命令自身耗时明显大于 0。
lt0=$(date +%s)
lfout=$(timeout 20 env HOME=$LGH "$WBOX_ABS" logs -f lgc 2>&1); lfrc=$?
lt1=$(date +%s)
lwait=$((lt1 - lt0))
lfcnt=$(printf '%s\n' "$lfout" | grep -c '^line')
if [ "$lfrc" -eq 0 ] && [ "$lfcnt" -eq 8 ] && [ "$lwait" -ge 1 ]; then
  report PASS "LG.2 -f 真的等到了后续输出（收齐 8 行，等待 ${lwait}s）"
elif [ "$lfrc" -eq 0 ] && [ "$lfcnt" -eq 8 ]; then
  report FAIL "LG.2 -f 跟随" "收齐 8 行但没等待（${lwait}s）——容器可能早已跑完，这一轮没真正验到跟随"
elif [ "$lfrc" -eq 124 ]; then
  report FAIL "LG.2 -f 自行结束" "容器已退出但跟随没结束（被 timeout 杀掉）"
else
  report FAIL "LG.2 -f 跟随" "rc=$lfrc 行数=$lfcnt（期望 8）"
fi

# 容器最后一段输出不能丢：跟随循环若"先读后判活"，会漏掉两者之间写下的内容，
# 而那一段往往正是失败原因。上面收齐 8 行已覆盖，这里再单独盯住末行。
if printf '%s\n' "$lfout" | grep -qx 'line8'; then
  report PASS "LG.3 跟随不丢容器退出前的最后一行输出"
else
  report FAIL "LG.3 末行完整性" "输出尾部: $(printf '%s' "$lfout" | tail -c 80 | tr '\n' '|')"
fi

# 已退出的容器加 -f 必须立即返回，不能挂住等一个永远不会来的写入
lfout=$(timeout 10 env HOME=$LGH "$WBOX_ABS" logs -f lgc 2>&1); lfrc=$?
if [ "$lfrc" -eq 0 ]; then
  report PASS "LG.4 对已退出容器 -f 立即返回（不挂住）"
else
  report FAIL "LG.4 已退出容器 -f" "rc=$lfrc（124 = 被 timeout 杀掉，说明挂住了）"
fi

HOME=$LGH "$WBOX_ABS" rm lgc >/dev/null 2>&1
rm -rf "$LGH"

echo
echo "=== RN wbox rename / prune（PRD F9.27）==="

# **这一组必须用自己的 HOME**：RN.6 要跑 `prune -f`，那会清掉该 HOME 下所有
# 已退出的记录。跟别的组共用 $WORK/home 的话，会顺手把它们的残留一起扫掉，
# 制造出跨组干扰——最难查的那类偶发红。
# 用宿主程序模式（`-- /bin/...`）而不是镜像：rename/prune 只碰状态记录，
# 根本不需要镜像，于是这个独立 HOME 里没有镜像缓存也无所谓。
RNH=$WORK/rnhome
rm -rf "$RNH" && mkdir -p "$RNH"
HOME=$RNH "$WBOX_ABS" run -d --name rnlive -- /bin/sleep 30 >/dev/null 2>&1
HOME=$RNH "$WBOX_ABS" run -d --name rndead -- /bin/echo hi >/dev/null 2>&1
sleep 2

# 改名要**连记录里的名字一起改**：只改目录的话 ps 还显示旧名，
# 目录名与记录名各说各话，比不支持改名更让人困惑。
nout=$(HOME=$RNH "$WBOX_ABS" rename rndead rnrenamed 2>&1); nrc=$?
if [ "$nrc" -eq 0 ] \
   && HOME=$RNH "$WBOX_ABS" ps -a | awk '$1=="rnrenamed"{f=1} END{exit !f}' \
   && ! HOME=$RNH "$WBOX_ABS" ps -a | awk '$1=="rndead"{f=1} END{exit !f}'; then
  report PASS "RN.1 rename 改掉目录与记录里的名字（ps 只认得新名）"
else
  report FAIL "RN.1 rename 生效" "rc=$nrc 输出: $(printf '%s' "$nout" | head -c 150)"
fi

# 改名后日志还得读得到：日志随状态目录一起搬，读不到等于把 logs 废掉
nout=$(HOME=$RNH "$WBOX_ABS" logs rnrenamed 2>&1); nrc=$?
if [ "$nrc" -eq 0 ] && printf '%s' "$nout" | grep -q hi; then
  report PASS "RN.2 改名后日志仍可读（记录整体搬走，不是重建）"
else
  report FAIL "RN.2 改名后 logs" "rc=$nrc 输出: $(printf '%s' "$nout" | tr '\n' ' ' | head -c 120)"
fi

# 运行中的容器必须拒绝改名，且要说清为什么（名字被用在可写层路径等地方）
nout=$(HOME=$RNH "$WBOX_ABS" rename rnlive rnother 2>&1); nrc=$?
if [ "$nrc" -ne 0 ] && printf '%s' "$nout" | grep -q '正在运行' \
   && HOME=$RNH "$WBOX_ABS" ps | awk '$1=="rnlive"{f=1} END{exit !f}'; then
  report PASS "RN.3 拒绝给运行中的容器改名，且原容器不受影响"
else
  report FAIL "RN.3 运行中拒绝改名" "rc=$nrc 输出: $(printf '%s' "$nout" | head -c 150)"
fi

# 目标名已被占用时要拒绝：静默覆盖会把另一个容器的记录整个抹掉
nout=$(HOME=$RNH "$WBOX_ABS" rename rnrenamed rnlive 2>&1); nrc=$?
if [ "$nrc" -ne 0 ] && HOME=$RNH "$WBOX_ABS" ps -a | awk '$1=="rnrenamed"{f=1} END{exit !f}'; then
  report PASS "RN.4 目标名被占用时拒绝改名（不覆盖别的容器记录）"
else
  report FAIL "RN.4 重名拒绝" "rc=$nrc"
fi

# prune 不加 -f 时**一条都不能删**。默认就删的话，一次手误没掉一批记录。
nout=$(HOME=$RNH "$WBOX_ABS" prune 2>&1); nrc=$?
if [ "$nrc" -eq 0 ] && printf '%s' "$nout" | grep -q rnrenamed \
   && HOME=$RNH "$WBOX_ABS" ps -a | awk '$1=="rnrenamed"{f=1} END{exit !f}'; then
  report PASS "RN.5 prune 不加 -f 时只列清单、不删任何东西"
else
  report FAIL "RN.5 prune 预演" "rc=$nrc 记录还在=$(HOME=$RNH "$WBOX_ABS" ps -a | awk '$1=="rnrenamed"{f=1} END{print (f?"是":"否")}')"
fi

# -f 才真删，且运行中的一个都不许碰
nout=$(HOME=$RNH "$WBOX_ABS" prune -f 2>&1); nrc=$?
if [ "$nrc" -eq 0 ] \
   && ! HOME=$RNH "$WBOX_ABS" ps -a | awk '$1=="rnrenamed"{f=1} END{exit !f}' \
   && HOME=$RNH "$WBOX_ABS" ps | awk '$1=="rnlive"{f=1} END{exit !f}'; then
  report PASS "RN.6 prune -f 清掉已退出记录，运行中的容器分毫未动"
else
  report FAIL "RN.6 prune 执行" "rc=$nrc 输出: $(printf '%s' "$nout" | tr '\n' '|' | head -c 150)"
fi

HOME=$RNH "$WBOX_ABS" kill rnlive >/dev/null 2>&1
for _ in 1 2 3 4 5 6 7 8 9 10; do
  HOME=$RNH "$WBOX_ABS" ps 2>/dev/null | awk '$1=="rnlive"{f=1} END{exit !f}' || break
  sleep 0.5
done
HOME=$RNH "$WBOX_ABS" rm rnlive >/dev/null 2>&1
rm -rf "$RNH"

echo
echo "=== RT wbox restart（PRD F9.26）==="

# 判据是**换了一条命 + 配置照旧**：容器 PID 必须变（真的重起了，不是没动），
# 且原来的运行配置仍然生效。只断言 rc=0 的话，一个什么都不做的实现也能过。
# 用与其它组同一个 HOME：镜像缓存在那里，另起一个干净 HOME 会因为找不到
# lbetest 而整组失败（吃过一次亏）。
RTH=$WORK/home
HOME=$RTH "$WBOX_ABS" run -d --name rtc --hostname rtbox lbetest -- /bin/sleep 30 >/dev/null 2>&1
sleep 2
rt1=$(HOME=$RTH "$WBOX_ABS" ps | awk '$1=="rtc"{print $3}')
rout=$(HOME=$RTH "$WBOX_ABS" restart rtc 2>&1); rrc=$?
sleep 2
rt2=$(HOME=$RTH "$WBOX_ABS" ps | awk '$1=="rtc"{print $3}')
if [ "$rrc" -eq 0 ] && [ -n "$rt1" ] && [ -n "$rt2" ] && [ "$rt1" != "$rt2" ]; then
  report PASS "RT.1 restart 换了一条命（PID $rt1 → $rt2）且容器仍在运行"
else
  report FAIL "RT.1 restart 生效" "rc=$rrc 前=$rt1 后=$rt2（应都非空且不同）"
fi

# 原配置要照旧生效：--hostname 是 run 时给的，重启后必须还在
rhost=$(HOME=$RTH "$WBOX_ABS" exec rtc -- /bin/hostname 2>&1)
if printf '%s' "$rhost" | grep -qx 'rtbox'; then
  report PASS "RT.2 重启后沿用原运行配置（--hostname 仍生效）"
else
  report FAIL "RT.2 沿用原配置" "容器内 hostname=$(printf '%s' "$rhost" | tr '\n' ' ' | head -c 80)（期望 rtbox）"
fi

# 名字只该出现一次：restart 内部要调 stop，但那一步不该也打一遍名字
if [ "$(printf '%s\n' "$rout" | grep -c '^rtc$')" -eq 1 ]; then
  report PASS "RT.3 restart 只汇报一次容器名（内部 stop 不重复打印）"
else
  report FAIL "RT.3 输出" "输出: $(printf '%s' "$rout" | tr '\n' '|' | head -c 120)"
fi

# 已退出的容器也能 restart（docker 语义）。这一条同时验证了 `run -d` 起的容器
# 确实记下了启动配置——补这条之前只有 create 的容器能再启动。
HOME=$RTH "$WBOX_ABS" kill rtc >/dev/null 2>&1
sleep 1
rout=$(HOME=$RTH "$WBOX_ABS" restart rtc 2>&1); rrc=$?
sleep 2
rt3=$(HOME=$RTH "$WBOX_ABS" ps | awk '$1=="rtc"{print $3}')
if [ "$rrc" -eq 0 ] && [ -n "$rt3" ]; then
  report PASS "RT.4 已退出的容器也能 restart（run -d 的启动配置被记住了）"
else
  report FAIL "RT.4 重启已退出容器" "rc=$rrc PID=$rt3 输出: $(printf '%s' "$rout" | head -c 150)"
fi

# start 同样受益：这是同一份配置的另一个入口
HOME=$RTH "$WBOX_ABS" kill rtc >/dev/null 2>&1
sleep 1
rout=$(HOME=$RTH "$WBOX_ABS" start rtc 2>&1); rrc=$?
sleep 2
rt4=$(HOME=$RTH "$WBOX_ABS" ps | awk '$1=="rtc"{print $3}')
if [ "$rrc" -eq 0 ] && [ -n "$rt4" ]; then
  report PASS "RT.5 start 也能拉起 run -d 起过的已退出容器"
else
  report FAIL "RT.5 start 已退出容器" "rc=$rrc PID=$rt4 输出: $(printf '%s' "$rout" | head -c 150)"
fi

rout=$(HOME=$RTH "$WBOX_ABS" restart nosuchrt 2>&1); rrc=$?
if [ "$rrc" -ne 0 ]; then
  report PASS "RT.6 restart 不存在的容器时报错"
else
  report FAIL "RT.6 未知容器" "rc=$rrc"
fi

# 收尾必须**等它真的退出**再 rm：`rm` 按设计会拒绝运行中的容器，
# kill 返回不等于状态已翻成 exited。直接 kill 完就 rm，偶发会留下一个还在跑的
# rtc 污染后面按 `ps` 判断的用例——这正是本轮撞到过一次的偶发红。
# 用轮询而不是固定 sleep：固定睡多久都只是猜，而轮询判的是真实状态。
HOME=$RTH "$WBOX_ABS" kill rtc >/dev/null 2>&1
for _ in 1 2 3 4 5 6 7 8 9 10; do
  HOME=$RTH "$WBOX_ABS" ps 2>/dev/null | awk '$1=="rtc"{found=1} END{exit !found}' || break
  sleep 0.5
done
HOME=$RTH "$WBOX_ABS" rm rtc >/dev/null 2>&1
if HOME=$RTH "$WBOX_ABS" ps -a 2>/dev/null | awk '$1=="rtc"{found=1} END{exit !found}'; then
  report FAIL "RT.7 收尾清理" "rtc 未被清掉，会污染后面按 ps 判断的用例"
else
  report PASS "RT.7 收尾后不留 rtc 记录（不污染后续用例）"
fi

echo
echo "=== EX wbox export / import（PRD F9.25）==="

# export 导出的必须是**合并后的容器现状**：改过的以容器为准、删掉的不能复活、
# 新增的要在。只断言"tar 非空"证明不了任何事——把镜像原样打一份也非空。
mkdir -p "$CACHE/rootfs/exetc"
printf 'from-image\n' > "$CACHE/rootfs/exetc/conf"
printf 'from-image\n' > "$CACHE/rootfs/exetc/doomed"
HOME=$WORK/home "$WBOX_ABS" run -d --name exc lbetest -- \
  /bin/sh -c 'echo from-container > /exetc/conf; rm /exetc/doomed; echo x > /exadded.txt; sleep 30' >/dev/null 2>&1
sleep 2

EXTAR=$WORK/exported.tar
eout=$(HOME=$WORK/home "$WBOX_ABS" export -o "$EXTAR" exc 2>&1); erc=$?
if [ "$erc" -eq 0 ] && [ -s "$EXTAR" ]; then
  report PASS "EX.1 export 产出非空归档"
else
  report FAIL "EX.1 export" "rc=$erc 输出: $(printf '%s' "$eout" | head -c 150)"
fi

# 判据落在归档内容上：三类改动都要正确体现
EXD=$WORK/exdump
rm -rf "$EXD" && mkdir -p "$EXD"
tar -xf "$EXTAR" -C "$EXD" 2>/dev/null
if [ "$(cat "$EXD/exetc/conf" 2>/dev/null)" = "from-container" ] \
   && [ -f "$EXD/exadded.txt" ] \
   && [ ! -e "$EXD/exetc/doomed" ]; then
  report PASS "EX.2 导出的是合并后的容器现状（改动生效、新增在、删除未复活）"
else
  report FAIL "EX.2 分层合并" "conf=$(cat "$EXD/exetc/conf" 2>/dev/null) 新增=$([ -f "$EXD/exadded.txt" ] && echo 有 || echo 无) 删除的还在=$([ -e "$EXD/exetc/doomed" ] && echo 是 || echo 否)"
fi

# 导出的是**裸 rootfs**，不该带 wbox 自己的结构（那是 save 的事）
if [ ! -e "$EXD/wbox-image.json" ] && [ ! -d "$EXD/rootfs" ] && [ -d "$EXD/exetc" ]; then
  report PASS "EX.3 导出的是裸 rootfs（顶层直接是 /bin、/etc，无 wbox 特有结构）"
else
  report FAIL "EX.3 裸 rootfs 形态" "顶层: $(ls "$EXD" 2>/dev/null | tr '\n' ' ' | head -c 120)"
fi

# 暂存目录必须收拾干净：留着会在容器状态目录里悄悄多吃一份完整 rootfs
EXSTATE=$WORK/home/.wbox/run/exc/export-staging
if [ ! -e "$EXSTATE" ]; then
  report PASS "EX.4 export 后不留物化暂存目录"
else
  report FAIL "EX.4 暂存清理" "$EXSTATE 仍在（会悄悄吃掉一份磁盘）"
fi

# 往返：import 回来的镜像要**真能跑**，且带着容器当时的改动。
# 落到干净 HOME 才证明归档自带全部内容，而不是靠原机器的缓存。
EXHOME=$WORK/exhome
mkdir -p "$EXHOME"
eout=$(HOME=$EXHOME "$WBOX_ABS" import -t exmine:v1 "$EXTAR" 2>&1); erc=$?
vout=$(HOME=$EXHOME "$WBOX_ABS" run --rm --name exrun exmine:v1 -- \
  /bin/sh -c 'cat /exetc/conf; test -e /exetc/doomed && echo STILL || echo DELETED' 2>&1)
if [ "$erc" -eq 0 ] \
   && printf '%s' "$vout" | grep -qx 'from-container' \
   && printf '%s' "$vout" | grep -qx 'DELETED'; then
  report PASS "EX.5 export→import 往返后镜像可运行且保留容器当时的改动"
else
  report FAIL "EX.5 往返" "import rc=$erc 运行输出: $(printf '%s' "$vout" | tr '\n' ' ' | head -c 180)"
fi

# import 缺 -t 要报错并指出带身份的归档该用 load——这两条命令最容易被搞混
eout=$(HOME=$EXHOME "$WBOX_ABS" import "$EXTAR" 2>&1); erc=$?
if [ "$erc" -ne 0 ] && printf '%s' "$eout" | grep -q 'wbox load'; then
  report PASS "EX.6 import 缺 -t 时报错并指出 save 归档该用 load"
else
  report FAIL "EX.6 缺 -t" "rc=$erc 输出: $(printf '%s' "$eout" | head -c 150)"
fi

# 路径穿越：裸 tar 是任意来源的外部输入，这是这条链路唯一真正危险的东西
EVIL=$WORK/evil.tar
( cd "$WORK" && printf 'pwned\n' > evilfile && tar -cf "$EVIL" --transform 's|^evilfile|../../../../tmp/wbox-pwned|' evilfile 2>/dev/null )
if [ -s "$EVIL" ]; then
  rm -f /tmp/wbox-pwned
  eout=$(HOME=$EXHOME "$WBOX_ABS" import -t evil:v1 "$EVIL" 2>&1); erc=$?
  if [ "$erc" -ne 0 ] && [ ! -e /tmp/wbox-pwned ]; then
    report PASS "EX.7 import 拒绝路径穿越归档且未落盘到 rootfs 之外"
  else
    report FAIL "EX.7 路径穿越" "rc=$erc 穿越文件落盘=$([ -e /tmp/wbox-pwned ] && echo 是 || echo 否)"
  fi
  rm -f /tmp/wbox-pwned
else
  report SKIP "EX.7 路径穿越" "本机 tar 不支持 --transform，造不出穿越归档"
fi

HOME=$WORK/home "$WBOX_ABS" kill exc >/dev/null 2>&1
HOME=$WORK/home "$WBOX_ABS" rm exc >/dev/null 2>&1
rm -rf "$CACHE/rootfs/exetc" "$EXD" "$EXTAR" "$EVIL"

echo
echo "=== SL save / load（PRD F9.22）==="

SLTAR=$WORK/saved.tar
sout=$(HOME=$WORK/home "$WBOX_ABS" save -o "$SLTAR" lbetest 2>&1); src=$?
if [ "$src" -eq 0 ] && [ -s "$SLTAR" ]; then
  report PASS "SL.1 save 产出非空归档"
else
  report FAIL "SL.1 save" "rc=$src 输出: $(printf '%s' "$sout" | head -c 150)"
fi

# 载入到**干净的 HOME**：这才证明归档自带全部内容，而不是靠原机器的缓存
SLHOME=$WORK/loadhome
mkdir -p "$SLHOME"
sout=$(HOME=$SLHOME "$WBOX_ABS" load -i "$SLTAR" 2>&1); src=$?
SLD=$SLHOME/.wbox/images/registry-1.docker.io/library_lbetest/latest
if [ "$src" -eq 0 ] && [ -x "$SLD/rootfs/bin/busybox" ] && [ -L "$SLD/rootfs/bin/sh" ] \
   && [ -f "$SLD/config.json" ]; then
  report PASS "SL.2 load 到干净 HOME 后内容与符号链接均还原"
else
  report FAIL "SL.2 load 还原" "rc=$src busybox=$([ -x "$SLD/rootfs/bin/busybox" ] && echo 有 || echo 无) sh软链=$([ -L "$SLD/rootfs/bin/sh" ] && echo 有 || echo 无)"
fi

# 还原出来的镜像必须**真能跑**——文件都在不等于镜像可用
sout=$(HOME=$SLHOME "$WBOX_ABS" run --rm --name slrun lbetest -- /bin/echo LOADED_OK 2>&1); src=$?
if [ "$src" -eq 0 ] && printf '%s' "$sout" | grep -q LOADED_OK; then
  report PASS "SL.3 载入的镜像可直接运行"
else
  report FAIL "SL.3 载入后可运行" "rc=$src 输出: $(printf '%s' "$sout" | head -c 150)"
fi

# -t 改名落地
sout=$(HOME=$SLHOME "$WBOX_ABS" load -i "$SLTAR" -t slrenamed:v9 2>&1); src=$?
if [ "$src" -eq 0 ] && [ -d "$SLHOME/.wbox/images/registry-1.docker.io/library_slrenamed/v9/rootfs" ]; then
  report PASS "SL.4 load -t 改名落地"
else
  report FAIL "SL.4 -t 改名" "rc=$src 输出: $(printf '%s' "$sout" | head -c 150)"
fi

# 不是 wbox 产的归档要明确报错，并指出逃生口
tar cf "$WORK/bogus.tar" -C "$WORK" saved.tar 2>/dev/null
sout=$(HOME=$SLHOME "$WBOX_ABS" load -i "$WORK/bogus.tar" 2>&1); src=$?
if [ "$src" -ne 0 ] && printf '%s' "$sout" | grep -q "wbox save"; then
  report PASS "SL.5 拒绝非 wbox save 产出的归档"
else
  report FAIL "SL.5 非法归档" "rc=$src 输出: $(printf '%s' "$sout" | head -c 150)"
fi

# save 不写 stdout：镜像很大，误写进终端代价太高，要明确要求 -o
sout=$(HOME=$WORK/home "$WBOX_ABS" save lbetest 2>&1); src=$?
if [ "$src" -ne 0 ] && printf '%s' "$sout" | grep -q "\-o"; then
  report PASS "SL.6 save 缺 -o 时报错（不默认写 stdout）"
else
  report FAIL "SL.6 缺 -o" "rc=$src 输出: $(printf '%s' "$sout" | head -c 150)"
fi
rm -f "$SLTAR" "$WORK/bogus.tar"

echo
echo "=== P 容器状态与 ps（PRD F8.1）==="

# 状态目录写在 HOME 下，hrun/run 都已把 HOME 指到 $WORK，天然与宿主隔离。
PSHOME=$WORK/home
wps() { HOME=$PSHOME "$WBOX_ABS" ps "$@" 2>&1; }

# P.1 运行中的容器要能被列出。用 setsid 起一个长命令，再从另一个进程查询
# ——这正是 ps 的意义：跨进程发现，而不是自己看自己。
HOME=$PSHOME setsid "$WBOX_ABS" run --name pstest -- /bin/sleep 30 >/dev/null 2>&1 &
sleep 2
if wps | grep -q "pstest.*running"; then
  report PASS "P.1 ps 列出运行中的容器（跨进程发现）"
else
  report FAIL "P.1 ps 列出运行中" "$(wps | tr '\n' ' ' | head -c 200)"
fi

# P.2 重名必须被拒，不能覆盖（覆盖会让"我以为在跑的那个"悄悄消失）
dup=$(HOME=$PSHOME "$WBOX_ABS" run --name pstest -- /bin/true 2>&1); duprc=$?
if [ "$duprc" -ne 0 ] && printf '%s' "$dup" | grep -q "正在使用中"; then
  report PASS "P.2 同名且存活时拒绝启动"
else
  report FAIL "P.2 重名拒绝" "rc=$duprc 输出: $(printf '%s' "$dup" | head -c 150)"
fi

# P.3 崩溃残留必须被如实标为 exited。SIGKILL 掉 wbox，RAII 的 Drop 不会执行，
# 状态目录会留下——此时判活只能靠锁，而锁已随进程消失。这条同时验证了
# "不靠 pid 判活"：pid 还可能被别人复用，锁不会。
psp=$(python3 -c "import json;print(json.load(open('$PSHOME/.wbox/run/pstest/meta.json'))['pid'])" 2>/dev/null)
if [ -n "$psp" ]; then
  kill -9 "$psp" 2>/dev/null
  sleep 1
  if [ -d "$PSHOME/.wbox/run/pstest" ] && wps -a | grep -q "pstest.*exited"; then
    report PASS "P.3 wbox 崩溃后残留被标为 exited（判活靠锁不靠 pid）"
  else
    report FAIL "P.3 崩溃残留标注" "$(wps -a | tr '\n' ' ' | head -c 200)"
  fi
  # 默认视图不该混入已退出的
  if ! wps | grep -q "pstest.*exited"; then
    report PASS "P.4 默认视图只列运行中（-a 才含已退出）"
  else
    report FAIL "P.4 默认视图" "$(wps | tr '\n' ' ' | head -c 150)"
  fi
else
  report SKIP "P.3/P.4 崩溃残留" "取不到 pstest 的 pid"
fi

# P.6 rm 必须拒绝运行中的容器。rm 只删记录、停不掉容器，真删了等于把一个
# 还在跑的容器变成 ps 看不见的孤儿。
HOME=$PSHOME setsid "$WBOX_ABS" run --name rmlive -- /bin/sleep 30 >/dev/null 2>&1 &
sleep 2
rmout=$(HOME=$PSHOME "$WBOX_ABS" rm rmlive 2>&1); rmrc=$?
if [ "$rmrc" -ne 0 ] && printf '%s' "$rmout" | grep -q "仍在运行"; then
  report PASS "P.6 rm 拒绝删除运行中的容器"
else
  report FAIL "P.6 rm 拒绝运行中" "rc=$rmrc 输出: $(printf '%s' "$rmout" | head -c 150)"
fi

# P.7 已退出的记录能被 rm 掉——这是 ps -a 里那些残留的唯一清理手段
rmp=$(python3 -c "import json;print(json.load(open('$PSHOME/.wbox/run/rmlive/meta.json'))['pid'])" 2>/dev/null)
if [ -n "$rmp" ]; then
  kill -9 "$rmp" 2>/dev/null; sleep 1
  if HOME=$PSHOME "$WBOX_ABS" rm rmlive >/dev/null 2>&1 && [ ! -d "$PSHOME/.wbox/run/rmlive" ]; then
    report PASS "P.7 rm 清掉已退出的残留记录"
  else
    report FAIL "P.7 rm 清理残留" "目录仍在或 rm 失败"
  fi
else
  report SKIP "P.7 rm 清理残留" "取不到 rmlive 的 pid"
fi

# P.8 重名报错必须给出**可行**的解法。此处容器还在跑，而 rm 明确拒绝运行中的
# 容器，所以文案里不该出现 `wbox rm`——那是指使用户去撞墙。
HOME=$PSHOME setsid "$WBOX_ABS" run --name advice -- /bin/sleep 20 >/dev/null 2>&1 &
sleep 2
adv=$(HOME=$PSHOME "$WBOX_ABS" run --name advice -- /bin/true 2>&1)
if printf '%s' "$adv" | grep -q "换个 --name" && ! printf '%s' "$adv" | grep -q "wbox rm"; then
  report PASS "P.8 重名报错给的是可行解法（不指向必被拒的 rm）"
else
  report FAIL "P.8 重名报错文案" "$(printf '%s' "$adv" | head -c 200)"
fi
advp=$(python3 -c "import json;print(json.load(open('$PSHOME/.wbox/run/advice/meta.json'))['pid'])" 2>/dev/null)
[ -n "$advp" ] && kill -9 "$advp" 2>/dev/null

# ---- F8.2 --detach / logs ----

# P.9 --detach 必须**立即返回**并打印容器名，容器继续在后台跑。
t0=$(date +%s)
dname=$(HOME=$PSHOME "$WBOX_ABS" run -d --name bg1 -- /bin/sh -c 'echo OUT1; echo ERR1 >&2; sleep 20' 2>&1)
t1=$(date +%s)
if [ "$dname" = bg1 ] && [ $((t1-t0)) -le 3 ]; then
  report PASS "P.9 --detach 立即返回并打印容器名"
else
  report FAIL "P.9 --detach 返回" "输出='$dname' 耗时=$((t1-t0))s"
fi

sleep 2
if HOME=$PSHOME "$WBOX_ABS" ps | grep -q "bg1.*running"; then
  report PASS "P.10 detach 的容器在后台持续运行"
else
  report FAIL "P.10 后台运行" "$(HOME=$PSHOME "$WBOX_ABS" ps | tr '\n' ' ' | head -c 200)"
fi

# P.11 stdout/stderr 分别落盘且可读
o=$(HOME=$PSHOME "$WBOX_ABS" logs bg1 2>&1)
e=$(HOME=$PSHOME "$WBOX_ABS" logs bg1 --stderr 2>&1)
if printf '%s' "$o" | grep -q OUT1 && printf '%s' "$e" | grep -q ERR1 \
   && ! printf '%s' "$o" | grep -q ERR1; then
  report PASS "P.11 logs 分别读取 stdout / stderr"
else
  report FAIL "P.11 logs 读取" "out='$(printf '%s' "$o"|head -c 80)' err='$(printf '%s' "$e"|head -c 80)'"
fi

bgp=$(python3 -c "import json;print(json.load(open('$PSHOME/.wbox/run/bg1/meta.json'))['pid'])" 2>/dev/null)
[ -n "$bgp" ] && kill -9 "$bgp" 2>/dev/null
sleep 1

# P.12 后台容器退出后**保留**记录与日志——这正是 logs 的主要用途（事后查看
# 一个已经跑完的后台任务到底输出了什么）。前台容器则仍是退出即清理（P.5）。
HOME=$PSHOME "$WBOX_ABS" run -d --name bg2 -- /bin/sh -c 'echo AFTER_EXIT' >/dev/null 2>&1
sleep 2
if HOME=$PSHOME "$WBOX_ABS" ps -a | grep -q "bg2.*exited" \
   && HOME=$PSHOME "$WBOX_ABS" logs bg2 2>/dev/null | grep -q AFTER_EXIT; then
  report PASS "P.12 detach 容器退出后仍保留记录与日志（可事后查看）"
else
  report FAIL "P.12 退出后保留" "$(HOME=$PSHOME "$WBOX_ABS" ps -a | tr '\n' ' ' | head -c 200)"
fi

# P.13 日志体积必须有界。这条专挑**短命暴写**的容器：它一秒内写完就退出，
# 活不到看门狗的第一次 tick，只靠周期检查会让整份输出原样落盘（实测 2.5MB）。
HOME=$PSHOME WBOX_LOG_MAX_BYTES=4096 "$WBOX_ABS" run -d --name flood \
  -- /bin/sh -c 'i=0; while [ $i -lt 200000 ]; do echo "x $i"; i=$((i+1)); done' >/dev/null 2>&1
sleep 4
fsz=$(stat -c%s "$PSHOME/.wbox/run/flood/stdout.log" 2>/dev/null || echo 0)
if [ "$fsz" -gt 0 ] && [ "$fsz" -lt 200000 ] \
   && grep -q "已丢弃此前内容" "$PSHOME/.wbox/run/flood/stdout.log" 2>/dev/null; then
  report PASS "P.13 日志体积有界且截断可见（短命暴写也挡得住）"
else
  report FAIL "P.13 日志上限" "大小=$fsz（应远小于 200000 且含截断标记）"
fi

# ---- F8.3 stop ----

# P.15 stop 必须收走**整棵进程树**，不能只杀掉直接子进程。guest 里故意再起
# 几个孙进程；判据沿用 L3 的思路——按 comm 精确数，不用 pgrep -f（那会把
# wbox 自己的 argv 也算进去，本地就误报过一次）。
# 标记必须**每次运行唯一**：这里按脚本自身 pid 派生一个睡眠时长。用固定值
# （原先是 777）会被上一次**中断**的运行留下的残留进程污染——实测踩过：
# 一次超时中断后，下一次 before 数到 6、after 数到 3，报出一个并不存在的
# "收树失败"。判据本身被污染，比功能坏掉更难查。
SLEEPMARK=$((700 + $$ % 90))
sleeps() { ps -eo comm,args --no-headers | awk -v m="$SLEEPMARK" '$1=="sleep" && index($0,m)' | wc -l; }
HOME=$PSHOME "$WBOX_ABS" run -d --name tree \
  -- /bin/sh -c "sleep $SLEEPMARK & sleep $SLEEPMARK & sleep $SLEEPMARK; wait" >/dev/null 2>&1
sleep 2
before=$(sleeps)
HOME=$PSHOME "$WBOX_ABS" stop tree >/dev/null 2>&1; strc=$?
sleep 1
after=$(sleeps)
if [ "$strc" -eq 0 ] && [ "$before" -ge 3 ] && [ "$after" -eq 0 ]; then
  report PASS "P.15 stop 收走整棵进程树（$before → $after）"
else
  report FAIL "P.15 stop 收树" "rc=$strc 之前=$before 之后=$after（期望 >=3 → 0）"
fi

# P.16 stop 之后状态应变为 exited，且日志仍在（detach 语义：退出后保留）
if HOME=$PSHOME "$WBOX_ABS" ps -a | grep -q "tree.*exited"; then
  report PASS "P.16 stop 后记录标为 exited 并保留"
else
  report FAIL "P.16 stop 后状态" "$(HOME=$PSHOME "$WBOX_ABS" ps -a | tr '\n' ' ' | head -c 200)"
fi

# P.17 stop 必须幂等：已经停了再停不算错，否则 `wbox stop x` 在脚本里没法用
if HOME=$PSHOME "$WBOX_ABS" stop tree >/dev/null 2>&1; then
  report PASS "P.17 stop 对已停止的容器幂等（不报错）"
else
  report FAIL "P.17 stop 幂等" "第二次 stop 返回非 0"
fi
HOME=$PSHOME "$WBOX_ABS" rm tree >/dev/null 2>&1

# P.18 停不存在的容器要明确报错（与幂等不冲突：那是"没这个东西"，不是"已停"）
if ! HOME=$PSHOME "$WBOX_ABS" stop nosuch >/dev/null 2>&1; then
  report PASS "P.18 stop 不存在的容器时报错"
else
  report FAIL "P.18 stop 不存在容器" "本应报错却成功"
fi

# ---- F8.4 exec（Linux 侧，PRD §4.9 L1）----

HOME=$PSHOME "$WBOX_ABS" run -d --name ebox -- /bin/sleep 40 >/dev/null 2>&1
sleep 2

# P.19 exec 必须进容器的 **PID 视图**。这条最容易假通过：只 setns 不再 fork
# 时命令照样能跑、netns 也对，唯独 $$ 是宿主的大号 pid——即"看着进去了，其实
# 没进"。setns(CLONE_NEWPID) 与 unshare 同理，只对之后创建的子进程生效。
epid=$(HOME=$PSHOME "$WBOX_ABS" exec ebox -- /bin/sh -c 'echo $$' 2>&1 | tr -d '\r')
if [ -n "$epid" ] && [ "$epid" -lt 100 ] 2>/dev/null; then
  report PASS "P.19 exec 进入容器 PID 视图（\$\$=$epid，非宿主大号 pid）"
else
  report FAIL "P.19 exec PID 视图" "得到 '\$\$'=$epid（期望容器内小号）"
fi

# P.20 exec 必须与容器同一 netns，否则"进容器"只是错觉
hostnet=$(readlink /proc/self/ns/net)
enet=$(HOME=$PSHOME "$WBOX_ABS" exec ebox -- /bin/sh -c 'readlink /proc/self/ns/net' 2>&1 | tr -d '\r')
if [ -n "$enet" ] && [ "$enet" != "$hostnet" ]; then
  report PASS "P.20 exec 与容器同一 netns（≠ 宿主）"
else
  report FAIL "P.20 exec netns" "exec=$enet 宿主=$hostnet（不该相同）"
fi

# P.21 退出码原样转发（双 fork 的中间进程要正确回传，容易漏）
HOME=$PSHOME "$WBOX_ABS" exec ebox -- /bin/sh -c 'exit 7' >/dev/null 2>&1
if [ $? -eq 7 ]; then
  report PASS "P.21 exec 退出码转发 (7)"
else
  report FAIL "P.21 exec 退出码" "得到 $?（期望 7）"
fi

# P.23 top 必须列出隔离单元里的 guest，而不是宿主侧 supervisor。
etop=$(HOME=$PSHOME "$WBOX_ABS" container top ebox 2>&1); etoprc=$?
esupervisor=$(python3 -c "import json;print(json.load(open('$PSHOME/.wbox/run/ebox/meta.json'))['pid'])" 2>/dev/null)
if [ "$etoprc" -eq 0 ] && printf '%s' "$etop" | grep -q '/bin/sleep 40' \
   && ! printf '%s' "$etop" | grep -q "^$esupervisor "; then
  report PASS "P.23 top 列出容器 guest 且不混入 supervisor"
else
  report FAIL "P.23 top 进程视图" "rc=$etoprc supervisor=$esupervisor 输出: $(printf '%s' "$etop" | head -c 200)"
fi

# P.24 kill 默认立即强制终止；这里同时走 container 子命令别名。
HOME=$PSHOME "$WBOX_ABS" container kill ebox >/dev/null 2>&1; ekillrc=$?
sleep 1
if [ "$ekillrc" -eq 0 ] && HOME=$PSHOME "$WBOX_ABS" ps -a | grep -q "ebox.*exited"; then
  report PASS "P.24 kill 立即终止并保留 exited 记录"
else
  report FAIL "P.24 kill" "rc=$ekillrc 状态: $(HOME=$PSHOME "$WBOX_ABS" ps -a | tr '\n' ' ' | head -c 150)"
fi

# P.22 容器已退出时必须**明确拒绝**。放行的后果不是报错而是更糟：命令会跑在
# 宿主上，而用户以为它在容器里。
eout=$(HOME=$PSHOME "$WBOX_ABS" exec ebox -- /bin/true 2>&1); erc=$?
if [ "$erc" -ne 0 ] && printf '%s' "$eout" | grep -q "已退出"; then
  report PASS "P.22 exec 对已退出容器明确拒绝"
else
  report FAIL "P.22 exec 拒绝已退出" "rc=$erc 输出: $(printf '%s' "$eout" | head -c 150)"
fi
HOME=$PSHOME "$WBOX_ABS" rm ebox >/dev/null 2>&1

# P.25 create 只保存配置，start 才执行；exited 后同一配置可再次启动。
create_marker=$WORK/create-start.marker
rm -f "$create_marker"
cout=$(HOME=$PSHOME "$WBOX_ABS" container create --name cstart --workdir "$WORK" \
  -- /bin/sh -c 'echo STARTED >> create-start.marker; sleep 30' 2>&1); crc=$?
if [ "$crc" -eq 0 ] && [ ! -e "$create_marker" ] \
   && HOME=$PSHOME "$WBOX_ABS" ps -a | grep -q "cstart.*created.*0"; then
  report PASS "P.25A create 保存配置但不执行 workload"
else
  report FAIL "P.25A create" "rc=$crc marker=$(test -e "$create_marker" && echo yes || echo no) 输出: $(printf '%s' "$cout" | head -c 150)"
fi
HOME=$PSHOME "$WBOX_ABS" container start cstart >/dev/null 2>&1
sleep 2
first=$(grep -c STARTED "$create_marker" 2>/dev/null || true)
HOME=$PSHOME "$WBOX_ABS" kill cstart >/dev/null 2>&1
HOME=$PSHOME "$WBOX_ABS" start cstart >/dev/null 2>&1
sleep 2
second=$(grep -c STARTED "$create_marker" 2>/dev/null || true)
if [ "$first" -eq 1 ] && [ "$second" -eq 2 ] \
   && HOME=$PSHOME "$WBOX_ABS" ps | grep -q "cstart.*running"; then
  report PASS "P.25B start 可运行并重启同一保存配置（$first → $second）"
else
  report FAIL "P.25B start/rerun" "marker 次数 $first → $second；状态: $(HOME=$PSHOME "$WBOX_ABS" ps -a | tr '\n' ' ' | head -c 150)"
fi
HOME=$PSHOME "$WBOX_ABS" kill cstart >/dev/null 2>&1
HOME=$PSHOME "$WBOX_ABS" rm cstart >/dev/null 2>&1

# P.14 前台容器没有日志文件时，要说清楚"只有 --detach 才落盘"，
# 不能让用户以为容器没有输出。
HOME=$PSHOME "$WBOX_ABS" run --name fglog -- /bin/echo hi >/dev/null 2>&1
lg=$(HOME=$PSHOME "$WBOX_ABS" logs fglog 2>&1)
if printf '%s' "$lg" | grep -qE "没有名为|--detach"; then
  report PASS "P.14 前台容器读 logs 时给出可懂解释"
else
  report FAIL "P.14 前台 logs 文案" "$(printf '%s' "$lg" | head -c 150)"
fi

# P.5 正常退出不留残留：RAII 应当把状态目录删干净
HOME=$PSHOME "$WBOX_ABS" run --name psclean -- /bin/true >/dev/null 2>&1
if [ ! -d "$PSHOME/.wbox/run/psclean" ]; then
  report PASS "P.5 正常退出后状态目录已清理"
else
  report FAIL "P.5 正常退出清理" "$PSHOME/.wbox/run/psclean 仍在"
fi

echo
echo "==================================="
echo "结果: PASS=$pass FAIL=$fail SKIP=$skip"
[ "$fail" -eq 0 ]
