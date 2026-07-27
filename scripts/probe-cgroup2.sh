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
#     待验：cgroup v2 的 "no internal process" 规则要求——父 cgroup 里
#     有进程时，不能对它 enable subtree_control 把控制器下发给子级；而不
#     enable，子级里根本不会出现 memory.max 这些文件。
#
# **移动进程必须用 sudo**：cgroup v2 迁移进程要求对**源与目的的共同祖先**也有
# 写权限。我们只 chown 了自己造的子树，源却在 root 拥有的
# /system.slice/... 下，共同祖先是根 cgroup —— 于是所有
# `echo pid > cgroup.procs` 一律 EACCES。上一版没意识到这点，把这个纯权限
# 错误当成了 "no internal process" 规则的证据，脚本还据此打印了
# "布局 A 不可能成立" —— 那个结论**不成立**，已撤回。
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
  sudo rmdir "$BASE"/*/*/*/ "$BASE"/*/*/ "$BASE"/*/ 2>/dev/null
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
moved_in=0
if sudo sh -c "echo $victim > '$BASE/a/cgroup.procs'" 2>/tmp/probe.err; then
  moved_in=1
  echo "  [ok]   把测试进程 $victim 移入 $BASE/a（经 sudo：跨 root 拥有的共同祖先）"
else
  echo "  [FAIL] 连 sudo 都移不进 $BASE/a —— $(tr -d '\n' </tmp/probe.err)"
fi
echo "  a/cgroup.procs 实际内容：$(cat "$BASE/a/cgroup.procs" 2>/dev/null | tr '\n' ' ')"
# 关键一步：$BASE/a 里**确实有进程**时，还能不能给子级下发控制器？
# 只有 moved_in=1 时这条结论才有意义 —— 空 cgroup 当然能 enable，
# 拿空 cgroup 的成功去说"含进程也能 enable"是上一版犯的错。
if [ "$moved_in" = 1 ]; then
  if echo "+memory" > "$BASE/a/cgroup.subtree_control" 2>/tmp/probe.err; then
    echo "  [!!]   **含进程**的 cgroup 仍可 enable subtree_control —— 布局 A 可能可行"
  else
    echo "  [预期] **含进程**的 cgroup 无法 enable subtree_control —— $(tr -d '\n' </tmp/probe.err)"
    echo "         => 这才是 no-internal-process 规则的直接证据"
  fi
else
  echo "  [跳过] 进程没进去，对 subtree_control 的判断无意义（不下结论）"
fi
mkdir -p "$BASE/a/child" 2>/dev/null
echo "  $BASE/a/child 内可见文件：$(ls "$BASE/a/child" 2>/dev/null | tr '\n' ' ')"
if echo $((16*1024*1024)) > "$BASE/a/child/memory.max" 2>/tmp/probe.err; then
  echo "  [ok]   布局 A 能写 memory.max"
else
  echo "  [FAIL] 布局 A 写不了 memory.max —— $(tr -d '\n' </tmp/probe.err)"
fi
# 反向再验一次：先给空的 a2 下发控制器，再试着往里塞进程。
# 若这一步被拒，就说明"进程与被限额子 cgroup 同父"在 cgroup v2 下**根本不可能**
# ——两个方向都堵死：有进程就不能 enable，enable 了就不能进进程。
say "布局 A 反向验证 —— 先 enable 再塞进程"
mkdir -p "$BASE/a2" 2>/dev/null
sudo chown -R "$(id -u):$(id -g)" "$BASE/a2" 2>/dev/null
if echo "+memory" > "$BASE/a2/cgroup.subtree_control" 2>/tmp/probe.err; then
  echo "  [ok]   空 cgroup 可 enable subtree_control"
  sleep 300 &
  v2=$!
  # 同样经 sudo，排除"共同祖先无写权限"这个与规则无关的干扰项
  if sudo sh -c "echo $v2 > '$BASE/a2/cgroup.procs'" 2>/tmp/probe.err; then
    echo "  [!!]   已 enable subtree_control 的 cgroup 仍可塞进程 —— 布局 A 也许可行"
  else
    echo "  [预期] 已 enable subtree_control 的 cgroup **拒绝**塞进程（sudo 也不行）—— $(tr -d '\n' </tmp/probe.err)"
    echo "         => 两个方向都堵死，布局 A 在 cgroup v2 下不成立"
  fi
  kill -9 "$v2" 2>/dev/null; wait "$v2" 2>/dev/null
else
  echo "  [FAIL] 空 cgroup 都 enable 不了 —— $(tr -d '\n' </tmp/probe.err)"
fi
rmdir "$BASE/a2" 2>/dev/null

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
sudo sh -c "echo $victim > '$BASE/b/target/cgroup.procs'" 2>/dev/null
echo "  target/cgroup.procs 内容：$(cat "$BASE/b/target/cgroup.procs" 2>/dev/null | tr '\n' ' ')"
kill -9 "$victim" 2>/dev/null; wait "$victim" 2>/dev/null
rmdir "$BASE/b/supervisor" "$BASE/b/target" "$BASE/b" 2>/dev/null

# ---------------- 真跑 wbox ----------------
say "真跑 wbox（在已委派的 cgroup 里）"
if [ ! -x "$WBOX" ]; then
  echo "  找不到 $WBOX，跳过"
  exit 0
fi
# 按**布局 B 的形状**搭：委派根 run/ 不放进程、只下发控制器；
# wbox 待在 run/supervisor 这个 leaf 里。这正是拟议中的修复形态 ——
# 先用探针验它成不成立，再决定要不要照这个改代码。
mkdir -p "$BASE/run/supervisor" 2>/dev/null
sudo chown -R "$(id -u):$(id -g)" "$BASE/run" 2>/dev/null
if echo "+memory +pids +cpu" > "$BASE/run/cgroup.subtree_control" 2>/tmp/probe.err; then
  echo "  [ok]   委派根 run/ 下发控制器（自身不放进程）"
else
  echo "  [FAIL] run/ 下发控制器失败 —— $(tr -d '\n' </tmp/probe.err)"
fi
# 关键问题：wbox 待在 supervisor 这个 leaf 里时，能不能在**兄弟**位置
# （run/ 之下）建一个 target 并写限额？能，则拟议的修复方向成立。
if mkdir -p "$BASE/run/target" 2>/dev/null && \
   echo $((16*1024*1024)) > "$BASE/run/target/memory.max" 2>/tmp/probe.err; then
  echo "  [ok]   **兄弟位置**的 target 可写 memory.max —— 拟议修复方向成立"
else
  echo "  [FAIL] 兄弟位置也写不了 memory.max —— $(tr -d '\n' </tmp/probe.err)"
fi
# 注意 `mypid=$$` 必须先在**外层 bash** 里取好：直接写
# `sudo sh -c "echo $$ > ..."` 取到的是那个临时 sh 的 pid，挪错进程。
out=$(bash -c "mypid=\$\$
               if sudo sh -c \"echo \$mypid > '$BASE/run/supervisor/cgroup.procs'\"; then
                 echo 'MOVED-IN ok（进了 supervisor leaf）'
               else
                 echo 'MOVED-IN FAILED —— 连 leaf 都进不去，本次无法验证 wbox'
               fi
               echo \"wbox 实际所在 cgroup: \$(sed -n 's/^0:://p' /proc/self/cgroup)\"
               exec '$PWD/$WBOX' run -V --memory 16 -- /bin/true" 2>&1)
rc=$?
echo "  rc=$rc"
echo "$out" | sed 's/^/  | /'
if echo "$out" | grep -q "限额（cgroup v2）"; then
  echo "  ==> wbox 走了 cgroup v2 首选路径"
elif echo "$out" | grep -q "rlimit 兜底"; then
  if echo "$out" | grep -q "MOVED-IN ok"; then
    echo "  ==> wbox **确实身处可写的委派 cgroup** 却仍退化到 rlimit"
    echo "      => 这才是 wbox 自身逻辑/布局的问题，值得改代码"
  else
    echo "  ==> wbox 退化到 rlimit，但它压根没被挪进委派 cgroup —— 本次不能归咎于 wbox"
  fi
else
  echo "  ==> 无法判断走了哪条路径"
fi
echo "  run/ 下现有：$(ls "$BASE/run" 2>/dev/null | tr '\n' ' ')"
echo "  supervisor 下现有：$(ls "$BASE/run/supervisor" 2>/dev/null | grep '^wbox-' | tr '\n' ' ')"

exit 0
