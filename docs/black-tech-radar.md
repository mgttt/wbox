# wbox 黑科技路线雷达

状态：探索基线（2026-08-02）。这是技术路线与证据地图，不把研究项冒充已交付能力。

## 1. 统一判断框架

每条路线用四个维度判断：

- **价值**：是否直接扩大 wbox 对 QEMU、WSL、Wine、Podman/Docker、Sandboxie、
  Parallels 的替代面。
- **依赖**：需要哪些现有 crate、宿主 ABI、硬件能力或新 provider。
- **证据**：从模型/fixture 到真机产品门禁需要跨过哪些级别。
- **下沉位置**：能力应进入 `wbox-machine`、`agenterm-platform`、wbox 核心，
  还是独立 crate。

证据等级：`E0` 假设，`E1` 设计/探针，`E2` 组件测试，`E3` 跨宿主门禁，
`E4` 产品黑盒与发布证据。`research` 不等于 `out`；只有违反无驱动、无第三方
runtime 或其他硬约束的实现路径才是 `out-by-constraint`。

## 2. 当前 substrate 锚点

```text
wbox-machine       host/ISA/硬件事实、provider 路由、artifact/matrix 身份
agenterm-platform  文件、进程、IPC、锁、句柄、原生 ABI 机制
wbox               provider 编排、OCI/CLI、隔离策略、产品生命周期
wbox-linux         ELF、CPU、Linux ABI、VFS、syscall/personality
future crates      state、journal、DBT、VMM、network、device、compatibility
```

边界原则：`wbox-machine` 决定“机器实际上能做什么”；`agenterm-platform` 决定
“如何安全调用宿主”；wbox 决定“用哪个 provider 执行什么 guest”；Agenterm
最终消费稳定机制和产品 contract。

## 3. 第一梯队：最值得立即深挖

| 路线 | 价值 | 主要依赖 | 首个可验证切片 | 下沉位置 | 当前级别 |
|---|---|---|---|---|---|
| Provider ladder | 让 native、user-mode、DBT、WHPX/KVM/HVF、full VM 共存 | `wbox-machine` route/provider、统一 state | 同一 ELF/OCI `RunSpec` 在两个 provider 上得到一致 lifecycle/receipt | `wbox-machine` + wbox provider SPI | E1 |
| Deterministic journal | 失败可复现、调试、回滚、未来迁移 | guest syscall、虚拟时间、随机数、fd、调度事件 | 记录并回放一个 shell/ELF fixture，输出 byte-identical receipt | future `wbox-journal` | E1 |
| Snapshot/COW state | 秒级启动、分支运行、恢复、迁移 | guest memory、VFS overlay、fd/CPU state | 进程 checkpoint 后 fork 两个分支，状态和文件层互不污染 | future `wbox-state` + `wbox-linux` | E1 |
| Capability broker | 把路径/权限变成不可伪造的 object capability | AppContainer SID、Job、typed handle、broker IPC | guest 只持 capability 访问文件/pipe，路径替换不能越界 | wbox broker；通用句柄仍下沉 platform | E2 |
| WSL1/WSL2 三档路线 | 覆盖 translation、managed VM、full-kernel guest | `wbox-machine` virtualization probe、wbox-linux、future VMM | 同一 workload 在 `portable-user-runtime` 与 `accelerated-vm` 报告统一能力矩阵 | route in `wbox-machine`; providers in wbox | E1 |

## 4. 第二梯队：形成替代壁垒

| 路线 | 黑科技点 | 依赖/风险 | 证据门槛 | 推荐归属 |
|---|---|---|---|---|
| DBT/hot translation | x86↔ARM、热点 block cache、运行时 ISA specialization | code cache invalidation、signals、self-modifying code | ISA corpus、性能对照、跨 ISA guest gate | future `wbox-dbt`，事实由 `wbox-machine` 提供 |
| WHPX/KVM/HVF provider | 同一 guest state 切换硬件加速 | hypervisor 可用性、admin/policy、设备模型 | probe、cold/warm boot、fallback、隔离与性能 receipt | future `wbox-vmm`; ABI 依赖 platform |
| crosvm/Firecracker 风格 microVM | 极简设备模型、低启动和强隔离 | kernel/image、virtio、VMM 安全边界 | Linux 真机 microVM、设备白名单、资源上限 | future `wbox-vmm` |
| PE/Win32 compatibility | 实质替代 Wine 的 CLI 子集 | loader、NT objects、threads、DLL、registry | 分阶段 PE fixtures；Linux/macOS 双宿主 gate | future `wbox-win32` |
| User-mode network provider | bridge、DNS、port mapping、pod 网络 | socket stack、NAT、隔离、常驻服务取舍 | namespace/provider matrix、流量和失败证据 | future `wbox-net` |
| Completion I/O fabric | IOCP/io_uring/kqueue/guest event 统一 | memory ordering、backpressure、fd ownership | stress、latency、lost-wakeup、跨宿主一致性 | `agenterm-platform` primitives + future crate |
| WASM/WASI guest | 更小、更可验证的第三 guest personality | component model、host capability mapping | deterministic WASI fixtures、resource limits、snapshot | future `wbox-wasm` |

## 5. 第三梯队：长期研究，不提前承诺

- **设备/图形 provider**：Virtio、GPU/NPU capability、DirectX/Vulkan/Metal；先做
  probe 与 ownership contract，再决定是否做 runtime。
- **远程/可迁移 guest**：把 state、journal、artifact identity 和 transport 组合成
  checkpoint transfer；必须先完成 deterministic replay 和 snapshot。
- **硬件机密计算**：TDX/SEV-SNP/Windows VBS 等只做 capability/provenance 研究，
  不把安全属性从探针推断成可用隔离。
- **用户态内核沙箱**：借鉴 gVisor 的分层 syscall boundary，但坚持第一方 Rust、
  不调用外部 sandbox runtime。

## 6. 依赖顺序

```text
wbox-machine facts/matrix
        │
        ├─► provider SPI + unified RunSpec/state
        │       ├─► deterministic journal
        │       ├─► snapshot/COW
        │       └─► native / user-mode / DBT / VMM providers
        │
        ├─► capability broker + typed handles
        └─► completion I/O fabric

wbox-linux / future PE / WASM guests
        └─► consume state, journal, VFS, broker and provider contracts
```

不得先做 provider-specific 优化再补统一 state；不得把 `wbox-machine` 降级成 CPU
信息工具；不得让 Agenterm UI/业务策略进入这些底层 crate。

## 7. 下一步建议

1. 先为 `RunSpec`、provider capability、state identity、journal event 建立最小中立
   contract，避免后续 DBT/VMM 各自发明状态模型。
2. 用一个小型 ELF shell fixture 先做 deterministic record/replay，再扩展 snapshot。
3. 在 `wbox-machine` 增加 portable/user-mode/accelerated/full-kernel 四档 route
   状态，而不是只返回一个 host capability boolean。
4. 对每条黑科技路线建立 E0→E4 证据清单；没有 E3/E4 的能力不得进入 `available`。
