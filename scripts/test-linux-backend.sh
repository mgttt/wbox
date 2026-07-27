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
  sleep 2
  alive=""
  for p in $tree; do
    [ "$p" = "$wb" ] && continue
    kill -0 "$p" 2>/dev/null && alive="$alive $p"
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
echo "==================================="
echo "结果: PASS=$pass FAIL=$fail SKIP=$skip"
[ "$fail" -eq 0 ]
