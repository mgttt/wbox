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

F9.1–F9.29 全部落地并有持续门禁。近期这一串是本轮做的：

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
| F9.22 `save`/`load` | SL.1–SL.6 | 打整个缓存目录（含 blobs，故搬过去仍可原样 push）；load 按白名单限定顶层条目防穿越 |
| F9.23 `wbox cp` | CP.1–CP.6 | 走 overlay 分层视图（读 upper→lower、写只写 upper），**不 setns**，故容器已退出也能取文件；必须认 whiteout，否则会把删掉的旧文件当现状拷出去 |
| F9.24 `wbox stats` | ST.1–ST.5 | cgroup 只在设了限额时存在，故两条路（cgroup / `/proc`）并**标注来源**；CPU% 采两次差值，分母用真实经过时间 |
| F9.25 `export`/`import` | EX.1–EX.7 | 与 `save`/`load` 搬的东西不同（裸 rootfs vs 镜像）；import 收任意来源归档，顶层无从白名单化，只能挡穿越 + 全落 `rootfs/` 下 |
| F9.26 `wbox restart` | RT.1–RT.7 | 顺带补了真实缺口：`run -d` 的容器此前不记启动配置，退出后连 `start` 都不行；`run-args.json` 与 `create.json` 必须分开，后者的存在本身是「该走 start」的标记 |
| F9.27 `rename`/`prune` | RN.1–RN.6 | rename 只对未运行容器开放（名字被用在可写层路径、默认主机名、Windows Job object 上，改名改不到这些）；prune 默认只列清单，`created` 不在清理范围 |
| F9.28 `logs -f`/`--tail` | LG.1–LG.4 | 跟随必须认日志截断（超上限会清零重写），否则之后静默哑掉；循环里先判活再读，反了会丢容器退出前的最后一段输出 |
| F9.29 `ps -q`/`rm -f` | RMF.1–RMF.5 | 一起补才凑齐 `rm -f $(ps -aq)` 清场惯用法；`-q` 只出名字（说明行会被当成容器名传下去）；`-f` 复用 stop 那条路 |

另外做了一次抽象收敛：七处"仅 Linux 可用"检查收敛到
`WboxError::require_linux(configured, flag, why)`（`src/error.rs`）。

CLI 参数层也做了一次：`start`/`rm`/`wait` 那种"一个或多个容器名、不收选项"的
解析各写过一遍、措辞互不相同（同一件事用户能看到好几种说法）；
`rm`/`prune`/`restart`/`compose down` 里"一个失败不中断后面的"这条取舍也各实现了
一遍。收敛成 `args::take_container_names` 与 `args::each_named`。
**收敛时踩到一条**：`rm` 有个测试断言的是旧措辞（"未能删除"），换共享措辞后红了
——那条断言本该盯行为而不是文案，已改成断言"运行中的容器记录确实还在"。

另一次收敛在 CLI 分发层：顶层分发、`container` 分发、`wbox help` 的主题判定、
给用户看的动词清单，四份清单原本各写各的，**已经漂开了**——`diff`/`commit`/
`cp`/`stats`/`pause` 能跑却不是"已知帮助主题"，`wbox help diff` 报错。收敛成
一张 `VERBS` 表（名字 + 作用域 + handler），另外三份全部由它派生；
`verb_table_is_the_only_source_of_truth` 把派生关系钉死。
**教训**：需要"每加一处要记得同步改 N 个地方"的设计，漂移只是时间问题，
而且不会有任何东西提醒你——发现时它已经错了很久了。

第三次收敛在分层视图：`diff`/`commit`/`cp`/`export` 问的是同一个问题——
"这个容器的文件系统由哪两层构成"——此前各解析一遍，连"没有 overlay 可写层"
那句错误说明都各抄了一份三处。收敛到 `src/layers.rs` 的 `ContainerLayers`：
`resolve`（一处措辞，调用方只说"我要干什么"）、`lookup`（读先 upper 后 lower，
认 whiteout）、`materialize`（硬链接铺下层 + 合并 upper，commit 与 export 共用）。

### 写门禁本身也会踩的坑（都是实际踩过的）

- **别跟别的组共用 HOME 做破坏性操作**。`prune -f` 会清掉该 HOME 下所有已退出
  记录；跟别的组共用 `$WORK/home` 就会顺手扫掉它们的残留，制造跨组干扰。
  RN 组因此独立 HOME，并改用宿主程序模式（rename/prune 只碰状态记录，
  不需要镜像，所以独立 HOME 里没有镜像缓存也无所谓）。
- **kill 完不能立刻 rm**。`rm` 按设计拒绝运行中的容器，而 kill 返回不等于状态
  已翻成 exited。直接 kill 完就 rm，偶发会留下还在跑的容器污染后面按 `ps`
  判断的用例（RT 组实测偶发红一次）。收尾要**轮询真实状态**再 rm，
  固定 sleep 睡多久都只是猜。
- **跑门禁前必须 `cargo build`**。门禁跑的是 `target/debug/wbox` 这个**文件**，
  而 `cargo test` 只构建测试二进制、不更新它。改完代码只跑 test 就跑门禁，
  测的是上一版程序——实测因此整组红过一次，报的是「未知参数」这种一看就知道
  跑错了版本的错。
- **判活别用 `kill -0`**。僵尸进程（已死、尚未被回收）照样返回成功。本机 PID 1
  不回收孤儿，`run -d` 的 supervisor 被 setsid 脱离后死掉就停在 `Z` 状态，
  于是 `kill -0` 把一个已经死了的进程报成活着——RMF.4 第一版就是这么假红的
  （产品没问题，判据错了）。改看 `/proc/<pid>/stat` 的状态字段，`Z` 视为已死。
- **判据要能排除「碰巧成立」**。LG.2 起初只数行数，可万一容器早已跑完，
  一次性读全也能凑够行数——那证明不了跟随。改成同时断言命令自身耗时 > 0，
  才是真的在验「它等到了后来才产生的输出」。

### 当前基线（接手时应能复现）

- `cargo test --locked` → **396 passed / 0 failed**
- `scripts/test-linux-backend.sh` → **189 PASS / 0 FAIL / 1 SKIP**
  （SKIP 是 cgroup v2 首选路径，需 `WBOX_LBE_CGROUP=1` + 已委派子树）
- `cargo clippy --locked --all-targets -- -D warnings` → 干净
- `cargo clippy --locked --target x86_64-pc-windows-gnu --all-targets -- -D warnings` → 干净
- `cargo check --locked --target x86_64-pc-windows-msvc` → 干净

**这四条是提交前的固定动作，一条都不能省。**

---

## 3. 下一步做什么

**Q3 的 F9 序列已全部做完**（F9.1–F9.29）。剩下的都在天花板之外或属另一象限：

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

### 4.2 基线要在**动作之后**采，别在动作之前

PZ.1 第一版拿 `pause` **之前**的计数当基线，结果偶发红：从采样到 pause 真正
生效之间有几十毫秒，容器照跑、计数 +1。要断言的本来就是"停下来之后不再变"，
那就该只看停下来之后的两个点。

同一形状的错误在本项目出现过多次——**判据里混进了动作生效之前的状态**。
写断言时先问一句：我采的这个数，是在我要验证的那件事发生之后吗？

### 4.3 定长 `sleep` 不能当判据

L3 收割检查曾用 `sleep 2` 然后看一眼 → 机器一忙就偶发红。改成**轮询到条件成立，
带上限**（判据没放松，到期仍失败照样 FAIL）。

### 4.4 静默降级是最坏的失败形态

凡是"做不到"的路径，一律**明确报错或出声告警**，绝不静默忽略：

- 非 Linux 宿主用 `-v`/`-p`/`--user`/`--cap-*`/`--seccomp-deny`/`--health-cmd`
  → 报错（现已统一走 `require_linux`）。
- 内核不支持 rootless overlay → **打印**回退说明，不静默共享写入。
- 拼错的 capability / syscall 名 → 报错。静默忽略会让用户**以为已经收紧了**。

### 4.5 探测要"真做一次"，别看特征文件

- cgroup v2 那次：用 `[ -f /sys/fs/cgroup/cgroup.controllers ]` 判断"能用"，
  结果 runner 上委派没开，实际走的是兜底路径 —— 探测说有、实际不能用。
- overlay 探测踩过同一形状的坑：**探测子进程必须写 uid_map**，光有 capability 不够
  （upper 的属主在未映射 ns 里是 overflow uid，过不了 overlayfs 属主校验）。
  手工 `unshare -Umr` 能过而进程内探测不过，差的就是 `-r` 那份映射。
- rootless overlay **必须带 `userxattr`**：不带时删**文件**正常，删**目录**直接
  `EIO`。只测文件的话这个缺陷完全看不出来——它在 F9.12 里潜伏了好几轮，
  是做 L5b 可行性实验时才撞出来的。判据要覆盖"删目录"。

### 4.6 断言要数**对的对象**：源码 ≠ 磁盘上的文件

`runtime` 的"原生代码欠债"棘轮走文件系统统计 C/H 文件，于是把构建产物也算了
进去——`vendor/blink/config.h`（被 .gitignore 忽略）与 `build-win32/version.h`
都是生成的，**在任何编译过 blink 的机器上这条断言都会红**，而欠债一点没变。
改成数 `git ls-files` 的输出。

同一个道理：要断言"仓库里有多少源码"，就去问版本库，别去问磁盘——
磁盘上还有别人（编译器、configure）放的东西。

### 4.7 起了服务当夹具时，必须确认**是你自己**绑上了端口

门禁里用 python3 起 registry stub 时踩过：先前手工测试遗留的进程还占着端口，
门禁自己的 stub **bind 失败**，用例照样连上了"那一个"服务器并得到看似合理的
结果，于是断言查不到本次运行的记录，报了一个与产品无关的假失败。

现在两个 stub 起完都会 `kill -0` 复核自己还活着，不活就直接 FAIL 并说明端口
可能被占。**另外：`ss -ltnp` 在这台机器上看不到监听项**，判断端口是否空闲要
用"真去 bind 一下"，别信 `ss` 的输出。

### 4.8 CI 与跨宿主

- **推之前等 CI**：曾因为 Windows 侧 `Drop` 顺序（删了还开着句柄的锁文件）
  在 CI 才暴露。
- 门禁曾"全绿却一个用例都没跑"：Ubuntu 24.04 起 AppArmor 默认关掉 unprivileged
  userns，脚本老实 SKIP 全部用例并返回 0。**CI 里必须设 `WBOX_LBE_REQUIRE=1`**，
  那时能力缺失记 FAIL 而不是 SKIP。
- msys 会改写命令行里的 guest 路径（`/busybox` → `D:/a/_temp/msys64/busybox`），
  造出纯自伤的假失败。Windows 侧脚本注意 `MSYS2_ARG_CONV_EXCL='*'` + `cygpath`。

### 4.9 内核/平台细节（踩过的）

- `unshare`/`setns` 带 `CLONE_NEWPID` **只影响之后创建的子进程** → 必须再 fork 一次。
  用 `/proc/<pid>/ns/pid_for_children`，不是 `ns/pid`。
- `mount(2)` 首次 bind **忽略 `MS_RDONLY`** → `:ro` 必须第二次
  `MS_BIND|MS_REMOUNT|MS_RDONLY`。漏了会静默变成可写。
- 日志文件必须 **append 打开**，否则截断被冲掉。
- netns 是**每线程**的；线程共享 fd 表，不需要 SCM_RIGHTS。
- 判活**以锁为准不以 pid 为准**：pid 会复用，`stop` 据此发信号就是杀错进程。

### 4.10 Python 生成 shell 脚本时

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
