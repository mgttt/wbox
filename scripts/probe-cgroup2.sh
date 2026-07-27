#!/usr/bin/env bash
# probe-cgroup2.sh —— cgroup v2 首选路径的**取证**脚本（不判定成败）。
#
# 背景：`memory.max`/`cpu.max`/`pids.max` 这条首选路径至今没有任何环境执行过
# （GitHub runner 有 v2 但委派未开，本仓开发容器是 cgroup v1），见
# docs-architecture.md §10.5「覆盖缺口」。在没证据之前不该猜它能不能用。
#
# 本脚本用 sudo 造一个**已委派**的 cgroup 子树，然后分别验证两种层级布局：
#
#   布局 A（wbox 当前做法）：进程和它要限额的子 cgroup 在同一个父级下
#       base/            <- wbox 进程在这里
#         └── child/     <- wbox 想在这里写 memory.max
#     预判会失败：cgroup v2 的 "no internal process" 规则要求——父 cgroup 里
#     有进程时，不能对它 enable subtree_control 把控制器下发给子级；而不
#     enable，子级里根本不会出现 memory.max 这些文件。
#
#   布局 B（runc/systemd 的做法）：进程先挪到旁边的 leaf，父级才好下发控制器
#       base/            <- 无进程，subtree_control = "+memory +pids +cpu"
#         ├── supervisor/  <- wbox 自己挪到这里
#         └── target/      <- 限额写这里，guest 加入这里
#
# 用法：scripts/probe-cgroup2.sh [wbox 二进制]
# 需要 sudo 与 cgroup v2。任何一步失败都只打印，不影响退出码（恒 0）——
# 这是取证不是门禁。
set -u

WBOX=${1:-target/debug/wbox}
BASE=/sys/fs/cgroup/wboxprobe-$$
say() { printf '\n--- %s\n' "$*"; }
try() { # try <描述> <命令...>
  local d=$1; shift
  if "$@" 2>/tmp/probe.err; then
    printf '  [ok]   %s\n' "$d"
  else
    printf '  [FAIL] %s —— %s\n' "$d" "$(tr -d '\n' </tmp/probe.err)"
  fi
}

if [ ! -f /sys/fs/cgroup/cgroup.controllers ]; then
  echo "本宿主不是 cgroup v2（无 /sys/fs/cgroup/cgroup.controllers），无法取证"
  exit 0
fi
if ! sudo -n true 2>/dev/null; then
  echo "无免密 sudo，无法造委派子树，跳过取证"
  exit 0
fi

say "环境事实"
echo "  uid=$(id -u)"
echo "  根 cgroup.controllers    = $(cat /sys/fs/cgroup/cgroup.controllers)"
echo "  根 cgroup.subtree_control= $(cat /sys/fs/cgroup/cgroup.subtree_control)"
echo "  自身 cgroup              = $(sed -n 's/^0:://p' /proc/self/cgroup)"

cleanup() {
  sudo rmdir "$BASE"/*/ 2>/dev/null
  sudo rmdir "$BASE" 2>/dev/null
}
trap cleanup EXIT

say "准备已委派的子树 $BASE"
try "mkdir $BASE" sudo mkdir -p "$BASE"
# 根 cgroup 不受 "no internal process" 约束，可以直接下发控制器
try "根 subtree_control += memory pids cpu" \
  sudo sh -c 'echo "+memory +pids +cpu" > /sys/fs/cgroup/cgroup.subtree_control'
try "chown $BASE 给当前用户（模拟委派）" sudo chown -R "$(id -u):$(id -g)" "$BASE"
# 控制器必须**逐级**下发：子 cgroup 里出现 memory.max，前提是它的**父级**在
# cgroup.subtree_control 里开了 memory。只开根的不够——首版探针漏了 $BASE
# 这一级，结果 b/target 里只有 cpu.stat/*.pressure，没有任何 *.max，
# 写入报 ENOENT，差点把"环境没搭对"误读成"布局 B 也不行"。
try "\$BASE subtree_control += memory pids cpu" \
  sh -c "echo '+memory +pids +cpu' > '$BASE/cgroup.subtree_control'"
echo "  $BASE/cgroup.controllers     = $(cat "$BASE/cgroup.controllers" 2>/dev/null)"
echo "  $BASE/cgroup.subtree_control = $(cat "$BASE/cgroup.subtree_control" 2>/dev/null)"

# ---------------- 布局 A：wbox 当前做法 ----------------
say "布局 A —— 进程与被限额的子 cgroup 同父（wbox 当前做法）"
mkdir -p "$BASE/a" 2>/dev/null
sudo chown -R "$(id -u):$(id -g)" "$BASE/a" 2>/dev/null
echo "  $BASE/a/cgroup.controllers = $(cat "$BASE/a/cgroup.controllers" 2>/dev/null)"
# 把一个**子进程**（不是本 shell，避免影响 CI runner 自身）挪进 $BASE/a
sleep 300 &
victim=$!
if echo "$victim" > "$BASE/a/cgroup.procs" 2>/tmp/probe.err; then
  echo "  [ok]   把测试进程 $victim 移入 $BASE/a"
else
  echo "  [FAIL] 移入 $BASE/a —— $(tr -d '\n' </tmp/probe.err)"
fi
# 关键一步：$BASE/a 里有进程，还能不能给子级下发控制器？
if echo "+memory" > "$BASE/a/cgroup.subtree_control" 2>/tmp/probe.err; then
  echo "  [ok]   含进程的 cgroup 仍可 enable subtree_control（与预判相反！）"
else
  echo "  [预期] 含进程的 cgroup 无法 enable subtree_control —— $(tr -d '\n' </tmp/probe.err)"
fi
mkdir -p "$BASE/a/child" 2>/dev/null
echo "  $BASE/a/child 内可见文件：$(ls "$BASE/a/child" 2>/dev/null | tr '\n' ' ')"
if echo $((16*1024*1024)) > "$BASE/a/child/memory.max" 2>/tmp/probe.err; then
  echo "  [ok]   布局 A 能写 memory.max"
else
  echo "  [FAIL] 布局 A 写不了 memory.max —— $(tr -d '\n' </tmp/probe.err)"
fi
kill -9 "$victim" 2>/dev/null; wait "$victim" 2>/dev/null
rmdir "$BASE/a/child" 2>/dev/null; rmdir "$BASE/a" 2>/dev/null

# ---------------- 布局 B：进程先挪到旁边的 leaf ----------------
say "布局 B —— 父级无进程，控制器下发给两个 leaf（runc/systemd 做法）"
mkdir -p "$BASE/b" 2>/dev/null
sudo chown -R "$(id -u):$(id -g)" "$BASE/b" 2>/dev/null
echo "  $BASE/b/cgroup.controllers = $(cat "$BASE/b/cgroup.controllers" 2>/dev/null)"
mkdir -p "$BASE/b/supervisor" "$BASE/b/target" 2>/dev/null
if echo "+memory +pids +cpu" > "$BASE/b/cgroup.subtree_control" 2>/tmp/probe.err; then
  echo "  [ok]   无进程的父级可 enable subtree_control"
else
  echo "  [FAIL] 无进程的父级也 enable 不了 —— $(tr -d '\n' </tmp/probe.err)"
fi
echo "  $BASE/b/target 内可见文件：$(ls "$BASE/b/target" 2>/dev/null | tr '\n' ' ')"
for f in memory.max pids.max cpu.max; do
  case $f in
    memory.max) v=$((16*1024*1024)) ;;
    pids.max)   v=8 ;;
    cpu.max)    v="50000 100000" ;;
  esac
  if echo "$v" > "$BASE/b/target/$f" 2>/tmp/probe.err; then
    echo "  [ok]   布局 B 写 $f = $v"
  else
    echo "  [FAIL] 布局 B 写不了 $f —— $(tr -d '\n' </tmp/probe.err)"
  fi
done
# 真把一个进程放进 target，确认限额确实生效
sleep 300 &
victim=$!
echo "$victim" > "$BASE/b/target/cgroup.procs" 2>/dev/null
echo "  target/cgroup.procs 内容：$(cat "$BASE/b/target/cgroup.procs" 2>/dev/null | tr '\n' ' ')"
kill -9 "$victim" 2>/dev/null; wait "$victim" 2>/dev/null
rmdir "$BASE/b/supervisor" "$BASE/b/target" "$BASE/b" 2>/dev/null

# ---------------- 真跑 wbox ----------------
say "真跑 wbox（在已委派的 cgroup 里）"
if [ ! -x "$WBOX" ]; then
  echo "  找不到 $WBOX，跳过"
  exit 0
fi
mkdir -p "$BASE/run" 2>/dev/null
sudo chown -R "$(id -u):$(id -g)" "$BASE/run" 2>/dev/null
# wbox 会在**自己所在的 cgroup**（这里是 run/）下建子目录写限额，
# 故 run 自己必须能把控制器下发给子级。它此刻还没有进程，应当能开。
if echo "+memory +pids +cpu" > "$BASE/run/cgroup.subtree_control" 2>/tmp/probe.err; then
  echo "  [ok]   run/ 下发控制器成功（wbox 进去之前）"
else
  echo "  [FAIL] run/ 下发控制器失败 —— $(tr -d '\n' </tmp/probe.err)"
fi
# 在子 shell 里把自己挪进 $BASE/run 再 exec wbox —— 这样 wbox 的
# /proc/self/cgroup 指向一个它有写权限的目录，正是 cgroup2_self_dir 要找的。
out=$(bash -c "echo \$\$ > '$BASE/run/cgroup.procs' 2>/dev/null;
               exec '$PWD/$WBOX' run -V --memory 16 -- /bin/true" 2>&1)
rc=$?
echo "  rc=$rc"
echo "$out" | sed 's/^/  | /'
if echo "$out" | grep -q "限额（cgroup v2）"; then
  echo "  ==> wbox 走了 cgroup v2 首选路径"
elif echo "$out" | grep -q "rlimit 兜底"; then
  echo "  ==> wbox 退化到 rlimit 兜底（首选路径仍未覆盖）"
else
  echo "  ==> 无法判断走了哪条路径"
fi
echo "  残留 cgroup：$(ls "$BASE/run" 2>/dev/null | grep -c '^wbox-' || echo 0) 个 wbox-* 目录"

exit 0
