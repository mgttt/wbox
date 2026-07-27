# HANDOFF —— 接手须知

写给**下一个会话的 agent**。目的是让你在不重读全部历史的前提下，知道：现在到哪了、
下一步做什么、以及这个项目里哪些坑是已经踩过、不要再踩的。

规范文档是 `PRD.md`，本文件不复述它，只做导航与交接。

---

## 1. 先读什么（按顺序，别跳）

1. `PRD.md` §1 阅读协议 → §2.4 四象限对标表 → §2.5 两条硬天花板。
   **对标不等于承诺功能对等**，每格逐条列了参照物能力与 wbox 实际状态。
2. `PRD.md` §4.9 `[TODO-PLAN]`：跨宿主协作的交接点。挑**与你这台机器宿主匹配**的
   条目做，别碰另一台的。
3. 本文件第 3 节（下一步）与第 4 节（血泪教训）。

---

## 2. 现在到哪了

### 协作现状

这个仓库**同时有两个 agent 在推进**：一个在 Linux 机器上（历史上的"我"），一个在
Windows 机器上。约定：

- **直接在 `main` 上工作**（用户明确要求）。推之前必 `git fetch origin main` +
  rebase，远程经常已经前进了几格。
- 冲突解决原则：**合并双方意图，不选边**。对方常常在做同一个抽象（例如
  `validate_options` 的提取），把自己的东西并进去而不是覆盖掉。
- 拿不到的机器**不硬猜**。无法在本机验证的东西不写进产品代码，改成
  `PRD.md` §4.9 的一个条目（说清背景、判据、怎样算做完）。这条是吃过亏的结论。

### 已完成（Linux 侧，Q3 对标 Podman/Docker）

F9.1–F9.14 全部落地并有持续门禁。近期这一串是本轮做的：

| 特性 | 门禁 | 一句话要点 |
|---|---|---|
| F9.7 `--user UID[:GID]` | U.1–U.4 | rootless 只能映射**一个** id，故直接把宿主 uid 映射成目标号，不是"当 root 再 setuid" |
| F9.8 `--cap-add/--cap-drop` | CAP.1–CAP.5 | 必须削 bounding set + 清 ambient；顺序：先 `PR_CAPBSET_DROP` 后 `capset` |
| F9.9 `--seccomp-deny` | SEC.1–SEC.6 | 是**拒绝名单**不是 docker 的允许名单，边界强度不同，PRD 已直说 |
| F9.10 `--health-cmd` | HC.1–HC.5 | 探针经 setns 跑在**容器内**（复用 exec 路径），不是宿主 |
| F9.11 `--network container:` | NC.1–NC.4 | user+net 必须一起加入（netns setns 要在属主 userns 有 CAP_SYS_ADMIN） |
| F9.12 overlay 可写层 | OV.1–OV.5 | 修了真实缺陷：此前容器写 `/` 会污染共享镜像缓存 |
| F9.13 `wbox push` | PSH.1–PSH.5 | 缓存无原始层 blob，只能 flatten 成单层；门禁用 python3 stub 闭环，不打真 registry |
| F9.14 compose 子集 | CMP.1–CMP.7 | 手写有界 YAML 子集（不引已归档的 serde_yaml）；up 复用 cmd_run 而非另写启动逻辑 |

另外做了一次抽象收敛：七处"仅 Linux 可用"检查收敛到
`WboxError::require_linux(configured, flag, why)`（`src/error.rs`）。

### 当前基线（接手时应能复现）

- `cargo test --locked` → **342 passed / 0 failed**
- `scripts/test-linux-backend.sh` → **109 PASS / 0 FAIL / 1 SKIP**
  （SKIP 是 cgroup v2 首选路径，需 `WBOX_LBE_CGROUP=1` + 已委派子树）
- `cargo clippy --locked --all-targets -- -D warnings` → 干净
- `cargo clippy --locked --target x86_64-pc-windows-gnu --all-targets -- -D warnings` → 干净
- `cargo check --locked --target x86_64-pc-windows-msvc` → 干净

**这四条是提交前的固定动作，一条都不能省。**

---

## 3. 下一步做什么

**Q3 的 F9 序列已全部做完**（F9.1–F9.14）。剩下的都在天花板之外或属另一象限：

- **镜像分层存储**（`FROM`/pull 仍整份复制）。注意与 F9.12 的运行期可写层是
  两件事。要做的话得让缓存额外保存原始压缩层 blob，牵动 pull/build/overlay/push
  四条路径。
- **pod**（多容器共享 IPC/UTS 的一等抽象）。F9.11 的共享 netns 已覆盖大半用途。
- 四象限里 Q1/Q2 的缺口属 Windows 侧，见 §4.9 W3。

§2.4 的四象限表已逐行核对过实况，并新增 **§2.4.1「每格的下一步」**——那张表
说明哪些缺口打算补、哪些永远不补（判断原则：要装驱动 / 要常驻服务 / 要虚拟化
的一律不补）。新立的两个条目：

- **L5 镜像分层存储**（待认领，Linux）：与 F9.12 的运行期可写层**不是一回事**，
  要改缓存布局保存原始压缩层 blob，牵动 pull/build/overlay/push 四条路径。
  判据：多层镜像 pull 后能原样 push 回去且 manifest digest 不变。
- **L6 pod**（待评估）：结论允许是"不做"。F9.11 已覆盖共享网络这个主要用途，
  要回答的是"去掉网络之后还剩多少真实需求"。

Q2 的 `-v` 由 Windows agent 在推进（broker 打开对象 HANDLE + Blink VFS 数据面，
**不走** OS 路径重定向），别去碰那块。

### ~~L3 `wbox push`~~ —— 已完成（F9.13，门禁 PSH.1–PSH.5）

保留下面的结论，因为"缓存里没有原始层 blob"这条仍然约束着**镜像分层存储**那一格：

- **缓存里没有原始层 blob**。布局是解包后的 `rootfs/` + `manifest.json` +
  `layers.json` + `config.json`，pull 时层 tar 解开后就丢了。所以**不能原样回推**。
- 可行路线是 **flatten**：把 `rootfs/` 打成单层 `tar.gz`，算 digest，生成单层
  manifest + config 再推。语义等价于 `docker commit` 后 push，**必须在文档里说清
  "推出去的是平铺单层，不保留原始分层"**。
- 依赖都在：`tar`、`flate2`（`rust_backend`，读写都有）、`sha2`、`serde_json`。

要动的地方：

- `src/oci/registry.rs` 目前**只有 GET**（`raw_get` / `get_authed`）。需要泛化成
  `raw_request(method, url, accept, content_type, body)`，加 `HEAD`/`POST`/`PUT`。
  Bearer 流程本身不用改：push 的 401 响应里 `WWW-Authenticate` 的 scope 自带
  `push,pull`，现有 `authenticate()` 照常解析。
- 上传三步：`POST /v2/<repo>/blobs/uploads/` 拿 Location →
  `PUT <location>&digest=...`（层与 config 各一次，先 `HEAD` 判存在可跳过）→
  `PUT /v2/<repo>/manifests/<tag>`。
- **凭证约束不能松**（PRD F7）：Basic 只发给与 registry 同 host 的 URL，realm 必须
  https 且同 host。现有 `url_host_matches` / `realm_host_allowed` 已经实现，复用。

门禁怎么写（这是难点，别拿真 registry 当门禁）：

- 用 python3 起一个**最小 registry stub**（收 POST/PUT、存内存、返回 202/201），
  断言收到的 manifest 与 blob digest 对得上；再用 `wbox pull` 从 stub 拉回来跑通，
  形成闭环。
- stub 是明文 http，而现有代码**强制 https**。需要加一个显式逃生口，建议
  `WBOX_INSECURE_REGISTRY=<host>`：仅当 registry host 精确匹配时才允许 http，
  且**明文时拒绝发送任何凭证**，并打印告警。这是安全相关改动，实现时把理由写进注释。

### ~~L4 compose 子集~~ —— 已完成（F9.14，门禁 CMP.1–CMP.7）

实现时改了当初的一个判断：**没有引 `serde_yaml`**（它已归档停维护，不再收安全
修复），改为手写有界 YAML 子集解析器并对不支持的构造逐条报错带行号。理由与
Dockerfile 子集解析器一致。

### 明确不做的（别去做，PRD 已列为天花板/非目标）

- Windows 侧文件系统写重定向做到 Sandboxie 级别 —— 要 minifilter 驱动，撞天花板一。
  用户态能逼近到什么程度是 §4.9 **W3，属 Windows agent**，且结论允许是"只能拒绝、
  不能重定向"。
- 自定义 bridge 网络 / 内建 DNS —— rootless 下要 slirp4netns/pasta 级常驻网络栈，
  与"免安装、无服务"（§2.2）冲突。
- 镜像**分层**存储（`FROM`/pull 仍整份复制）。注意它与 F9.12 的**运行期**可写层是
  两件事，PRD 已区分，别混。

---

## 4. 血泪教训（这个项目反复踩的坑）

这一节比功能清单更重要。**大部分红灯不是产品坏了，是判据错了。**

### 4.1 「判据通过了」≠「真的验证了目标」

本项目已至少四次因为**测试自身的错误**误判产品：

- `pgrep -c -f 'sleep 300'` 匹配到了 wbox 自己的 argv 和 pgrep 那个 shell
  → 假的"2 个残留"。改成匹配 `comm`。
- 日志上限"没生效"：watchdog 每 500ms 一 tick，而暴写容器活不到第一次 tick。
  产品没错，判据的时间假设错了。
- `COPY ../../etc/hostname` 那个文件不存在，`canonicalize` 先失败，
  **断言根本没执行到**。
- R.5 的 marker 落在宿主真实 `/` 上（宿主模式不换根），那里不可写 →
  看着像 restart/top 回归。**诊断时我还先错怪了 busybox 少链 `touch`。**

**结论**：红了先问"判据本身对吗"，尤其当失败形态是"看着像回归"的时候。写判据时优先
选**能区分两种世界**的探针（例如 HC.3 用"宿主有 `/home`、rootfs 没有"来证明探针跑在
容器里；NC.1 三方比对 ns inode 而不是只比两方）。

### 4.2 定长 `sleep` 不能当判据

L3 收割检查曾用 `sleep 2` 然后看一眼 → 机器一忙就偶发红。改成**轮询到条件成立，
带上限**（判据没放松，到期仍失败照样 FAIL）。

### 4.3 静默降级是最坏的失败形态

凡是"做不到"的路径，一律**明确报错或出声告警**，绝不静默忽略：

- 非 Linux 宿主用 `-v`/`-p`/`--user`/`--cap-*`/`--seccomp-deny`/`--health-cmd`
  → 报错（现已统一走 `require_linux`）。
- 内核不支持 rootless overlay → **打印**回退说明，不静默共享写入。
- 拼错的 capability / syscall 名 → 报错。静默忽略会让用户**以为已经收紧了**。

### 4.4 探测要"真做一次"，别看特征文件

- cgroup v2 那次：用 `[ -f /sys/fs/cgroup/cgroup.controllers ]` 判断"能用"，
  结果 runner 上委派没开，实际走的是兜底路径 —— 探测说有、实际不能用。
- overlay 探测踩过同一形状的坑：**探测子进程必须写 uid_map**，光有 capability 不够
  （upper 的属主在未映射 ns 里是 overflow uid，过不了 overlayfs 属主校验）。
  手工 `unshare -Umr` 能过而进程内探测不过，差的就是 `-r` 那份映射。

### 4.5 CI 与跨宿主

- **推之前等 CI**：曾因为 Windows 侧 `Drop` 顺序（删了还开着句柄的锁文件）
  在 CI 才暴露。
- 门禁曾"全绿却一个用例都没跑"：Ubuntu 24.04 起 AppArmor 默认关掉 unprivileged
  userns，脚本老实 SKIP 全部用例并返回 0。**CI 里必须设 `WBOX_LBE_REQUIRE=1`**，
  那时能力缺失记 FAIL 而不是 SKIP。
- msys 会改写命令行里的 guest 路径（`/busybox` → `D:/a/_temp/msys64/busybox`），
  造出纯自伤的假失败。Windows 侧脚本注意 `MSYS2_ARG_CONV_EXCL='*'` + `cygpath`。

### 4.6 内核/平台细节（踩过的）

- `unshare`/`setns` 带 `CLONE_NEWPID` **只影响之后创建的子进程** → 必须再 fork 一次。
  用 `/proc/<pid>/ns/pid_for_children`，不是 `ns/pid`。
- `mount(2)` 首次 bind **忽略 `MS_RDONLY`** → `:ro` 必须第二次
  `MS_BIND|MS_REMOUNT|MS_RDONLY`。漏了会静默变成可写。
- 日志文件必须 **append 打开**，否则截断被冲掉。
- netns 是**每线程**的；线程共享 fd 表，不需要 SCM_RIGHTS。
- 判活**以锁为准不以 pid 为准**：pid 会复用，`stop` 据此发信号就是杀错进程。

### 4.7 Python 生成 shell 脚本时

替换串以 `"` 结尾又紧挨 `"""` 时会多吐一个引号进脚本，`bash -n` 检查不出来，
只有运行时才炸。生成后**看一眼实际写进去的内容**。

---

## 5. 常用命令

```bash
# 四件套（提交前固定动作）
cargo test --locked
cargo clippy --locked --all-targets -- -D warnings
cargo clippy --locked --target x86_64-pc-windows-gnu --all-targets -- -D warnings
cargo check --locked --target x86_64-pc-windows-msvc

# Linux 端到端门禁（需要 ./busybox 静态二进制）
cargo build --locked && ./scripts/test-linux-backend.sh
# CI 语义（能力缺失记 FAIL 而非 SKIP）
WBOX_LBE_REQUIRE=1 ./scripts/test-linux-backend.sh

# 同步远程（每轮开始都做）
git fetch origin main && git rebase origin/main
```

门禁分组速查：L1/L2 隔离与限额、H 宿主模式、N/N2 网络与转发、C cgroup v2、
W wine、B 构建、V 卷、R 重启、U `--user`、CAP capability、SEC seccomp、
HC 健康检查、NC 容器间网络、OV overlay、PSH 镜像推送、CMP compose、P 生命周期。
