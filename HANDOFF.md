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

F9.1–F9.21 全部落地并有持续门禁。近期这一串是本轮做的：

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
| F9.15 IPC/UTS 隔离与共享 | IU.1–IU.7 | 修的是**隔离缺口**：此前容器直接用宿主的 IPC/UTS；顺带发现 exec 没进这两个 ns |
| F9.16 原始层留存 + 原样回推 | PSH.6–PSH.7 | pull 留一份压缩层，多层镜像 push 回去 digest 不变 |
| F9.17 构建产物分层 | PSH.8a–PSH.8c | build 产物 = 基础层 + 增量层，push 时基础层被跳过 |
| F9.18 FROM 硬链接共享 | OVB.1–OVB.4 | 磁盘共享数据块；靠 COPY unlink-first + RUN 走 overlay 保证基础镜像不被就地改写 |
| F9.19 `wbox diff` | DF.1–DF.3 | 直接读 overlay upper 得出 A/C/D，不扫全树；无 overlay 层时报错而非打印空清单 |
| F9.20 `wbox commit` | CM.1–CM.4 | **纯编排**：复用 F9.18 的硬链接+合并、F9.17 的分层 manifest，没加新机制 |
| F9.21 `pause`/`unpause` | PZ.1–PZ.3 | 信号而非 freezer（cgroup 只在设了限额时存在）；进程清单复用 `top` 的枚举 |

另外做了一次抽象收敛：七处"仅 Linux 可用"检查收敛到
`WboxError::require_linux(configured, flag, why)`（`src/error.rs`）。

### 当前基线（接手时应能复现）

- `cargo test --locked` → **356 passed / 0 failed**
- `scripts/test-linux-backend.sh` → **140 PASS / 0 FAIL / 1 SKIP**
  （SKIP 是 cgroup v2 首选路径，需 `WBOX_LBE_CGROUP=1` + 已委派子树）
- `cargo clippy --locked --all-targets -- -D warnings` → 干净
- `cargo clippy --locked --target x86_64-pc-windows-gnu --all-targets -- -D warnings` → 干净
- `cargo check --locked --target x86_64-pc-windows-msvc` → 干净

**这四条是提交前的固定动作，一条都不能省。**

---

## 3. 下一步做什么

**Q3 的 F9 序列已全部做完**（F9.1–F9.18）。剩下的都在天花板之外或属另一象限：

- **镜像分层存储**（`FROM`/pull 仍整份复制）。注意与 F9.12 的运行期可写层是
  两件事。要做的话得让缓存额外保存原始压缩层 blob，牵动 pull/build/overlay/push
  四条路径。
- **pod**（多容器共享 IPC/UTS 的一等抽象）。F9.11 的共享 netns 已覆盖大半用途。
- 四象限里 Q1/Q2 的缺口属 Windows 侧，见 §4.9 W3。

§2.4 的四象限表已逐行核对过实况，并新增 **§2.4.1「每格的下一步」**——那张表
说明哪些缺口打算补、哪些永远不补（判断原则：要装驱动 / 要常驻服务 / 要虚拟化
的一律不补）。新立的两个条目：

- ~~**L5 / L5b 镜像分层存储**~~：**已全部完成**（F9.16–F9.18）——pull 保留原始
  压缩层、多层镜像原样回推 digest 不变、build 产物写成基础层+增量层且 push 时
  基础层被跳过、`FROM` 硬链接共享数据块。
  **改动这块前务必记住那条纪律**：staging 与基础镜像缓存共享 inode，任何写入
  路径都必须"先 unlink 再落盘"或走 overlay，否则会写坏**别的**镜像。
  OVB.1 用全文件摘要直接盯这条。
- ~~**L6 pod**~~：**已评估，结论是不做**。评估过程发现 IPC/UTS 根本没隔离，
  于是先补了 F9.15；补齐后 pod 的三样共享都能单独取得，再抽一层只是换个说法。

Q2 当前最高优先级是 PRD §2.2.1/F4 的 Rust-only runtime 替换。不得继续修改
Blink/C 层，也不得恢复已撤回的 brokerfs 实验；卷数据面等待纯 Rust guest VFS。

**Q2 的 `-p` 已结案（§4.9 W5），别再当待办**：读 vendored 的 blink 源码就能定
——`HostfsSocket/Bind/Listen` 全部直落宿主 socket，仓库里没有自建网络栈，
所以 guest 绑的端口**就是**宿主端口，"映射进容器"这件事不成立。
顺带澄清了一个容易搞错的点：**Q2 的网络隔离模型与 Q3 不是一回事**——
Q3 靠 netns，Q2 靠 AppContainer 不授 `INTERNET_CLIENT`（能力开关，不是独立网络栈）。

这也说明一件事：**有些"要 Windows 才能查"的问题，其实读仓库里的 vendored 源码
就能定**。动手前先看看答案在不在本地。

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

- Windows 侧文件系统写重定向做到 Sandboxie 级别 —— §4.9 **W3**。**结构性分析已
  完成**（读本仓库代码即可得）：拒绝那一档已经免费成立（AppContainer + Low IL
  默认就拒绝，`acl.rs` 是在**打开**口子）；重定向那一档差的是**介入点**——Q2 有
  Blink VFS 在路径上，Q1 的原生 PE 程序发真 NT 调用，架构里没有任何东西经手，
  不注入挂钩就无从重定向。剩两条必须实机测：AppContainer 下 UAC VirtualStore
  还生不生效、per-package 存储对非 UWP 进程是否自动可写。**实验已经设计好并写进
  §4.9 W3**（含 PowerShell 步骤与逐条判据，注意用 `cmd.exe` 而不是 PowerShell
  ——后者自带 manifest 会被排除在 VirtualStore 外，测出假阴性），接手的人照跑、
  把原始输出和结论填回 §2.4 Q1 即可。
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
  → 假的"2 个残留"。改成匹配 `comm`。**这条踩了不止一次**：后来
  `pkill -9 -f stub2.py` 又匹配到自己那条命令，把整个 shell 杀掉，
  编辑没执行还看不出错。要按 PID/端口定位，别按 argv 关键字。
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
- rootless overlay **必须带 `userxattr`**：不带时删**文件**正常，删**目录**直接
  `EIO`。只测文件的话这个缺陷完全看不出来——它在 F9.12 里潜伏了好几轮，
  是做 L5b 可行性实验时才撞出来的。判据要覆盖"删目录"。

### 4.5 起了服务当夹具时，必须确认**是你自己**绑上了端口

门禁里用 python3 起 registry stub 时踩过：先前手工测试遗留的进程还占着端口，
门禁自己的 stub **bind 失败**，用例照样连上了"那一个"服务器并得到看似合理的
结果，于是断言查不到本次运行的记录，报了一个与产品无关的假失败。

现在两个 stub 起完都会 `kill -0` 复核自己还活着，不活就直接 FAIL 并说明端口
可能被占。**另外：`ss -ltnp` 在这台机器上看不到监听项**，判断端口是否空闲要
用"真去 bind 一下"，别信 `ss` 的输出。

### 4.6 CI 与跨宿主

- **推之前等 CI**：曾因为 Windows 侧 `Drop` 顺序（删了还开着句柄的锁文件）
  在 CI 才暴露。
- 门禁曾"全绿却一个用例都没跑"：Ubuntu 24.04 起 AppArmor 默认关掉 unprivileged
  userns，脚本老实 SKIP 全部用例并返回 0。**CI 里必须设 `WBOX_LBE_REQUIRE=1`**，
  那时能力缺失记 FAIL 而不是 SKIP。
- msys 会改写命令行里的 guest 路径（`/busybox` → `D:/a/_temp/msys64/busybox`），
  造出纯自伤的假失败。Windows 侧脚本注意 `MSYS2_ARG_CONV_EXCL='*'` + `cygpath`。

### 4.7 内核/平台细节（踩过的）

- `unshare`/`setns` 带 `CLONE_NEWPID` **只影响之后创建的子进程** → 必须再 fork 一次。
  用 `/proc/<pid>/ns/pid_for_children`，不是 `ns/pid`。
- `mount(2)` 首次 bind **忽略 `MS_RDONLY`** → `:ro` 必须第二次
  `MS_BIND|MS_REMOUNT|MS_RDONLY`。漏了会静默变成可写。
- 日志文件必须 **append 打开**，否则截断被冲掉。
- netns 是**每线程**的；线程共享 fd 表，不需要 SCM_RIGHTS。
- 判活**以锁为准不以 pid 为准**：pid 会复用，`stop` 据此发信号就是杀错进程。

### 4.8 Python 生成 shell 脚本时

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
HC 健康检查、NC 容器间网络、OV overlay、PSH 镜像推送、CMP compose、IU IPC/UTS、P 生命周期。
