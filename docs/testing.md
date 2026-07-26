# wbox 测试体系与发布门禁

wbox 的验证分三层：**Rust 单测**（纯逻辑，跨平台可跑）、**guest 测试**（tests/，
容器内行为级）、**shell 真机矩阵**（wbox-linux 在真 Windows 上的验收）。发布
（tag `v*`）由 CI 门禁把三层串起来：全绿才创建 GitHub Release。

## 一、三层测试

### 1. Rust 单测（`src/**` 内 `#[cfg(test)]`）

- 位置：就近写在各模块的 `mod tests`（`src/oci/*`、`src/backend/*`、`src/cli/*`、
  `src/testenv.rs`，以及 `cfg(windows)` 的 `src/sandbox.rs` 等）。
- 覆盖面：
  - `oci/mod.rs`——ImageRef 解析边界（digest 引用、registry 端口、多级路径、
    大写/localhost 现状、非法引用拒绝表）、缓存路径布局（`:` → `_`、缓存键含
    registry）；
  - `oci/config.rs`——Entrypoint × Cmd × 显式 cmd 八组合表驱动、畸形 config.json；
  - `oci/registry.rs`——realm host 校验表驱动（同 host/允许列表/跨 host/http
    拒绝/相似域名欺骗）、Basic 凭证作用域、authenticate 预网络失败路径；
  - `oci/image.rs`——digest 校验失败路径、manifest 平台选择、压缩格式分派、
    whiteout/opaque、路径穿越与 symlink 逃逸防护、硬链接；
  - `backend/env.rs`——保留键/白名单/脱敏全分支、优先级（forced > 镜像 > 宿主）；
  - `backend/mod.rs` + `main.rs`——classify_target 边界（`ubuntu` vs `./ubuntu`
    vs `ubuntu:latest` vs 绝对路径）、CLI 解析、临时 HOME 下的集成链
    （假缓存 list→show→classify→run-prepare）；
  - Windows 专属测试——AppContainer profile 生命周期与 capability SID、
    Job Object 限额下发，以及 AppContainer + Job + `CreateProcessW` 完整启动链。
    这些测试属于 `cargo test` 常规门禁，不允许以权限不足等理由静默跳过。
- 网络用例约定：真实 pull（hello-world）registry 不可达时 **SKIP 不 fail**
  （`eprintln!("SKIP：…")` 后返回 Ok）；严格失败场景一律用本地构造输入覆盖。
- 临时目录约定：用 `std::process::id()` + tag 拼唯一目录，测试末清理；
  需要 HOME 的用例经 `TempHome` 脚手架保存/恢复环境变量。
- **环境变量约定（硬性）**：`cargo test` 默认在同一进程内并行跑用例，而
  `std::env::set_var/remove_var` 改的是**进程级**全局状态——两个用例各自设
  `HOME`／`PATH`／`WBOX_LINUX` 必然互踩（曾导致 `integration_*` 与
  `image_rm_*` 随机失败：一方读到另一方的临时 HOME，甚至读到一半被对方的
  清理删掉）。因此：
  - 测试中**禁止**直接调用 `std::env::set_var/remove_var`，一律经
    `crate::testenv::EnvGuard`——构造时取全局环境锁（改环境的用例之间自动
    串行），Drop 时把每个触碰过的键还原到用例开始前的值（含"原本不存在→删除"），
    杜绝跨用例泄漏；
  - 需要临时 HOME 的用例用 `cli::TempHome`（内部已持有一把 `EnvGuard`）；
    若还需要别的变量，经 `TempHome::env()` 借出**同一把**守卫，
    不要另起 `EnvGuard`（同线程二次加锁会自死锁）；
  - 回归自查：`grep -rn "std::env::set_var\|std::env::remove_var" src/ |
    grep -v src/testenv.rs` 应为空。

### 2. guest 测试（`tests/`）

- 入口契约：**`tests/run.sh`**（退出码即测试结果）。该文件已落地，是
  `tests/run-guest-tests.sh` 的薄包装（透传参数、原样转发退出码）。
- 运行前置**两项，缺一不可**：
  1. `wbox-linux.exe`（被测运行时；由 `build-wbox-linux` 产出）；
  2. `zig`（`tests/guest/build.sh` 要把 `t_*.c` 交叉编译成
     **x86_64-linux-musl 静态 Linux ELF**，MinGW/MSVC 都做不到）。
- CI job `guest-tests`：`needs: build-wbox-linux`，取其 artifact 拿到 exe，
  并经 msys2 装 zig，然后真正执行套件。artifact 缺失时仍按契约
  `::notice::` SKIP 不阻塞。
- **不与矩阵 F 组重复**：CI 的矩阵步骤设 `WBOX_GUEST_SKIP=1` 整组跳过 F，
  guest 套件由本 job 独家承担。本地跑 `test-matrix.sh` 不设该变量，
  F 组照常执行（本地一条命令跑全量的便利保留）。
- **已知失败基线**（`tests/known-failures.txt`，机器可读）：套件本身就有
  可收紧的失败集合。runner 与基线比对：

  | 情况 | 退出码 | 含义 |
  |---|---|---|
  | 失败集合 ⊆ 基线 | 0 | 已知失败，放行（打印清单） |
  | 出现基线外的新失败 | 1 | **回归** |
  | 基线内的用例变成通过 | 1 | **基线过期**，必须同步收紧 |

  最后一条是刻意的，与 §四 "行为修复时测试会变红，强制同步更新" 同一原则——
  否则修好的东西会被基线继续掩盖。`WBOX_GUEST_NO_BASELINE=1` 可回到原始语义。

  真 Windows 基线为空，已达到 20/20 文件级通过；Wine 仅登记
  `t_net_sockopt @wine`。模式专有缺陷可用 `@native` / `@wine` 标注；
  真机首次执行发现的 W1（fork 挂死）与 W3
  （长路径），以及最后两项 N1（AF_UNIX）均已修复并从基线移除。
  当前真机断言级结果为 **1003 pass / 0 fail / 9 skip**；文件映射覆盖
  fork 后父窗口可见性、磁盘写回、私有/共享 mremap、exec 地址复用、
  MAP_FIXED 清理及内部 fd 0；另以 160 个并存 MAP_SHARED 文件映射验证
  动态注册表的写回、fork 快照克隆和父窗口同步；160 个匿名共享映射另行
  验证 Windows 临时文件 backing 也统一走 Fshare，而不依赖重复地址表。
  子进程在退出前执行 `msync`、`munmap` 或 `MAP_FIXED` 时也会按范围更新
  父窗口，测试分别覆盖仍存活时的即时可见性和条目提前移除后的可见性。
  稀疏文件回归另在 4 GiB+ offset 验证 `lseek`、`truncate`/`ftruncate`、
  MAP_SHARED 初始填充、mremap 扩容和 msync 写回，并确认低 32 位别名页
  不会被误读或覆盖；同一高位夹具还验证 sendfile 的显式 offset 指针与
  当前文件位置两种模式。fd 回归另直接在未写入数据的空管道上验证
  `pread`/`pwrite` 及其 vectored 版本立即返回 `ESPIPE`，由 runner 超时
  门禁防止阻塞回归；pipe 两端和 socket 的 `lseek` 也必须返回 `ESPIPE`，
  不得把 Win32 句柄或 Winsock 占位 fd 的位置 0 泄漏为成功结果。
- `t_fork_mem` 文件级结果之外，runner 会用
  `WBOX_TEST_FORK_FAIL=system|machine|args|thread` 各重跑一次轻量探针，
  验证 fork 建立失败不会损坏父窗口、误删父 VFS/Fshare 条目或阻断后续 fork。
  这四次故障注入不进入 `wtest` 断言统计，但任一失败都会把 `t_fork_mem`
  判为 FAIL。
- `t_mmap` 另由 runner 以 `WBOX_TEST_FSHARE_FAIL` 重跑五次轻量探针，
  覆盖 `msync`、`munmap` 首次写回失败、预写回后不重复 I/O，以及
  中间拆分分配失败和 `MAP_FIXED` 覆盖失败；要求错误返回 guest、原映射
  保持可用且重试后数据可落盘。这些探针同样不进入断言统计，任一失败都会
  判 `t_mmap` 为 FAIL。

### 3. shell 真机矩阵（`scripts/test-matrix.sh`）

- 内容：基础 11 项 + shell 8 项 + fork 矩阵 + wget md5 + epoll 单测（缺二进制
  自动 SKIP）。真 Windows 上 native 模式跑 wbox-linux.exe。
- 触发收窄：矩阵脚本无子集开关，故在 workflow 层收窄——**PR 只跑核心组**
  （wbox-linux.exe 构建 + 冒烟），完整矩阵留给 push main / tag / nightly /
  手动触发。
- **单项超时**：`WBOX_MATRIX_TIMEOUT`（默认 60s，设 0 关闭，`timeout`
  不可用时自动退化为不限制）。该上界用于把 fork 等运行时回归限制在单项内，
  避免一次挂死吃满整个 CI job。挂死记
  `rc=124` 并在详情里注明「超时 Ns 被终止」，其余项照常继续。
- CI 里两个环境开关（本地跑不设，全量执行）：
  - `WBOX_GUEST_SKIP=1`——F 组交给专职的 `guest-tests` job，避免重复；
  - `WBOX_MATRIX_NET_SKIP=1`——D 组从 guest 内 wget 公网，runner 到该站点的
    可达性不是被测对象，一次抖动就会弄红发布门禁。这与 `smoke-windows`
    的 `image pull`（registry 不可达记 `::warning::` 标黄不红）是同一条既有
    惯例。guest 侧 socket/epoll 语义不因此失去覆盖：`t_net_epoll` /
    `t_net_sockopt` 走本地 loopback，由 `guest-tests` 执行。

### 4. Linux 原生后端验收（`scripts/test-linux-backend.sh`）

- 覆盖 `docs-architecture.md` §10.5 的验收标准，走**完整 CLI 链路**
  （造假镜像缓存 → `wbox run`），而非只测内部函数：
  - **L1/L2**（镜像模式）：uid 映射 / 新根隔离 / PID namespace / 退出码转发 /
    `--memory` / `--max-procs` / `--cpu-pct`；
  - **H**（宿主程序模式，`wbox run -- <本机程序>`）：PID 1 / **不换根**
    （宿主文件系统可见，与镜像模式相反）/ `--workdir` 作工作目录 / 退出码 /
    工作目录无 `.wbox_oldroot` 残留；
  - **N**（网络默认）：默认断网、`--allow-network` 放行、断网时 loopback 仍可用；
  - **L3**（生命周期）：SIGKILL wbox 后进程树无残留，宿主/镜像两种模式各一条。
    判定方式是 kill 前收集 wbox 在**宿主视角**的全部后代 pid、kill 后逐个确认
    消失 —— 不按进程名匹配（既会误伤宿主同名进程，也会漏掉换根后名字不同的
    情况；先前拿重命名的 busybox 当标记，结果 busybox 按 argv[0] 分派 applet
    认不出这个名字直接退出，测出来是"guest 未起来"而不是"收割成功"）。
- **断言默认行为，不只断言"带上参数后有效"**。§10.5 记的两次红线违规
  （root 下 `--max-procs` 静默失效、Linux 侧默认联网）都是默认值不一致，
  两边都"能跑"，只有并排实测同一条命令才暴露。
- **进门禁的理由**：runner 以非 root 跑，`RLIMIT_NPROC` 兜底路径只有非 root
  才真实可测（**root 会绕过 RLIMIT_NPROC**，本地开发容器是 root，只能验
  "明确拒绝"分支）。
- **`WBOX_LBE_REQUIRE=1`（CI 必设）**：能力缺失记 FAIL 而非 SKIP。这条门禁
  上线时曾"全绿"却零覆盖——ubuntu runner 从 24.04 起由 AppArmor 关掉了
  unprivileged user namespace，脚本老实 SKIP 全部用例后返回 0。SKIP 语义没
  错，错在没区分"本地机器恰好不支持"与"专为这条门禁选的 runner 竟然不支持"。
  workflow 现在先 `sysctl` 打开该开关，再以 REQUIRE=1 跑。
- **断言实际发生的事，不按宿主特征猜能力**。原先用
  `[ -f /sys/fs/cgroup/cgroup.controllers ]` 当"有 cgroup v2"的判据，但那个
  文件存在**不代表能用**（runner 上委派未开，实际走兜底）。现在每项只要求
  落在"可接受结局集合"里：例如 `--cpu-pct` 要么真生效、要么明确拒绝，不接受
  静默忽略；`--max-procs` 要么挡住 fork 炸弹、要么明确拒绝。
- **cgroup v2 首选路径目前无任何环境覆盖**（runner 委派未开、本地容器是
  cgroup v1）。脚本每次打印 `note:` 说明本次覆盖了哪条路径，不靠猜。
  详见 `docs-architecture.md` §10.5「覆盖缺口」。
- 本地跑：`scripts/test-linux-backend.sh`（默认 `target/debug/wbox` + `./busybox`）。

## 二、本地跑法

```bash
# Rust 单测（Linux 177 项；Windows 另含 11 项真机 API/启动链测试）
cargo test --locked

# Windows 代码编译门禁（Win32 专属模块只在 windows target 编译）
rustup target add x86_64-pc-windows-msvc
cargo check --locked --target x86_64-pc-windows-msvc   # 要求 0 warning

# shell 真机矩阵（需真 Windows + msys2/Git Bash，或 Linux + wine）
scripts/test-matrix.sh vendor/blink/build-win32/wbox-linux.exe ./busybox
# 网络用例不可达保护：WBOX_MATRIX_NET_SKIP=1 scripts/test-matrix.sh ...

# guest 测试（入口落地后）
bash tests/run.sh

# Linux 原生后端验收：L1/L2 + 宿主程序模式 + 网络默认
# （需静态 busybox 与 unprivileged userns；网络那几条还需 python3）
scripts/test-linux-backend.sh
```

提交前最低门槛：`cargo test` 全绿 + **双目标 clippy 0 warning**：

```bash
cargo clippy --all-targets --locked -- -D warnings
cargo clippy --all-targets --locked --target x86_64-pc-windows-msvc -- -D warnings
```

两个目标缺一不可——`cfg(windows)` 模块（sandbox/token/job/acl）只在 windows
target 下参与编译，host 侧 clippy 看不到它们。该门槛由 CI 的
`check-windows-msvc` job 强制执行（此前只跑 `cargo check`，而 check 不因
warning 失败，标准形同虚设）。

## 三、发布门禁（`.github/workflows/ci.yml`）

```
tag v* push
  └─ release job（needs 全绿才执行）：
       ① test-linux          Rust 单测（Linux）
       ② test-windows        同一套单测在 windows 上跑——cfg(windows) 模块
                             （sandbox/acl/job/token）的测试只有这里编译得到
       ③ check-windows-msvc  Win32 编译门禁
       ④ smoke-windows       真机冒烟（AppContainer 链路）
       ⑤ build-wbox-linux    wbox-linux 构建 + 完整真机矩阵
       ⑥ guest-tests         guest 测试（前置未就绪 = SKIP，补齐后自动生效）
       ⑦ test-linux-backend  Linux 原生后端验收：L1/L2 + 宿主程序模式 +
                             网络默认（rootless namespace +
                             cgroup v2；ubuntu runner 才同时具备"非 root"与
                             "cgroup v2"两个前置，见 §一.4）
  └─ 产出：wbox.exe + wbox-linux.exe + wbox-portable-windows-x64.zip
           + SHA256SUMS.txt（两 exe + zip 的 sha256，必附）
```

- 任一门禁 job FAIL → 不创建 Release；guest-tests SKIP（黄）不算 FAIL。
- `test-linux` 与 `test-windows` 跑的是同一条 `cargo test --locked`；
  差别只在 target——Windows 专属模块在 Linux 上整段不编译，其单测
  自然也不会执行（`image_dir_layout_segments` 这类只在 Windows 上
  暴露的缺陷曾长期被 Linux CI 掩盖）。
- PR/push 常规 CI 保持快速：①②③ + ④的构建冒烟核心组；完整矩阵在
  push main / tag / nightly（每日 02:17 UTC）/ workflow_dispatch 跑。

## 四、KNOWN-FAILURES 约定

- guest C 套件（`tests/`）的已知失败走 `tests/known-failures.txt` 基线，
  语义见 §一.2；叙述与裁决理由留在 `tests/KNOWN-FAILURES.md`。两者必须
  同步：基线是门禁读的，Markdown 是人读的。
- Rust 单测的已知失败**不得**写成"预期失败但放行"的测试。约定做法二选一：
  1. 测试中断言**当前实际行为**并在注释中标注 `记录现状`（如大写引用不
     规范化、`--pull` 下 `C:\` 盘符冒号被当 tag 分隔符）——行为修复时测试
     会变红，强制同步更新；
  2. 用 `#[ignore = "KNOWN-FAILURE: <issue/原因>"]` 标注，并在提交信息中
     登记；`cargo test -- --include-ignored` 可全量复核。
- 禁止用 `#[should_panic]` 掩盖未知 panic；禁止静默 catch 真实 FAIL。
- 网络依赖用例只允许 SKIP（带 `::warning::`/`SKIP：` 日志），不允许假绿。
