# wbox 测试体系与发布门禁

wbox 的验证分三层：**Rust 单测**（纯逻辑，跨平台可跑）、**guest 测试**（tests/，
容器内行为级）、**shell 真机矩阵**（wbox-linux 在真 Windows 上的验收）。发布
（tag `v*`）由 CI 门禁把三层串起来：全绿才创建 GitHub Release。

## 一、三层测试

### 1. Rust 单测（`src/**` 内 `#[cfg(test)]`）

- 位置：就近写在各模块的 `mod tests`（`src/oci/*`、`src/backend/*`、`src/main.rs`）。
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
    （假缓存 list→show→classify→run-prepare）。
- 网络用例约定：真实 pull（hello-world）registry 不可达时 **SKIP 不 fail**
  （`eprintln!("SKIP：…")` 后返回 Ok）；严格失败场景一律用本地构造输入覆盖。
- 临时目录约定：用 `std::process::id()` + tag 拼唯一目录，测试末清理；
  需要 HOME 的用例经 `TempHome` 脚手架保存/恢复环境变量。

### 2. guest 测试（`tests/`）

- 由独立工作流维护；入口契约：**`tests/run.sh`**（退出码即测试结果）。
- CI job `guest-tests`：入口存在则执行；`tests/` 未就绪时记 `::notice::` SKIP
  并成功退出（不阻塞门禁）。入口一旦落地即自动并入发布门禁。

### 3. shell 真机矩阵（`scripts/test-matrix.sh`）

- 内容：基础 11 项 + shell 8 项 + fork 矩阵 + wget md5 + epoll 单测（缺二进制
  自动 SKIP）。真 Windows 上 native 模式跑 wbox-linux.exe。
- 触发收窄：矩阵脚本无子集开关，故在 workflow 层收窄——**PR 只跑核心组**
  （wbox-linux.exe 构建 + 冒烟），完整矩阵留给 push main / tag / nightly /
  手动触发。

## 二、本地跑法

```bash
# Rust 单测（Linux 即可跑全部；含网络 SKIP 语义）
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

提交前最低门槛：`cargo test` 全绿 + windows-msvc check 0 warning。

## 三、发布门禁（`.github/workflows/ci.yml`）

```
tag v* push
  └─ release job（needs 全绿才执行）：
       ① test-linux          Rust 单测
       ② check-windows-msvc  Win32 编译门禁
       ③ smoke-windows       真机冒烟（AppContainer 链路）
       ④ build-wbox-linux    wbox-linux 构建 + 完整真机矩阵
       ⑤ guest-tests         guest 测试（未就绪 = SKIP，就绪后自动生效）
  └─ 产出：wbox.exe + wbox-linux.exe + wbox-portable-windows-x64.zip
           + SHA256SUMS.txt（两 exe + zip 的 sha256，必附）
```

- 任一门禁 job FAIL → 不创建 Release；guest-tests SKIP（黄）不算 FAIL。
- PR/push 常规 CI 保持快速：①②③ + ④的构建冒烟核心组；完整矩阵在
  push main / tag / nightly（每日 02:17 UTC）/ workflow_dispatch 跑。

## 四、KNOWN-FAILURES 约定

- 已知失败用例**不得**写成"预期失败但放行"的测试。约定做法二选一：
  1. 测试中断言**当前实际行为**并在注释中标注 `记录现状`（如大写引用不
     规范化、`--pull` 下 `C:\` 盘符冒号被当 tag 分隔符）——行为修复时测试
     会变红，强制同步更新；
  2. 用 `#[ignore = "KNOWN-FAILURE: <issue/原因>"]` 标注，并在提交信息中
     登记；`cargo test -- --include-ignored` 可全量复核。
- 禁止用 `#[should_panic]` 掩盖未知 panic；禁止静默 catch 真实 FAIL。
- 网络依赖用例只允许 SKIP（带 `::warning::`/`SKIP：` 日志），不允许假绿。
