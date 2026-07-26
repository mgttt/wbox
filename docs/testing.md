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

  真 Windows 基线为空，已达到 19/19 文件级通过；Wine 仅登记
  `t_net_sockopt @wine`。模式专有缺陷可用 `@native` / `@wine` 标注；
  真机首次执行发现的 W1（fork 挂死）与 W3
  （长路径），以及最后两项 N1（AF_UNIX）均已修复并从基线移除。

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
