# AGI 算力基础设施前瞻计划

本文记录 wbox 在廉价高性能计算方向的长期假设、实验方法和演进路线。它不是
AGI 已可实现的声明，也不替代 `PRD.md` 的产品需求与进度树。

## 1. 长期目标

核心观察窗口是：

```text
有效 1 PFLOPS-hour / USD <= 1
```

这里的“有效”不能用芯片标称峰值代替：

```text
有效算力成本
= 总成本 /（标称算力 × 实际利用率 × 可用时间）
```

总成本至少包含计算、内存、存储、网络、能耗、编排、故障恢复和闲置。达到目标
只表示大规模 AGI 实验的算力门槛显著下降，不表示算法、数据、对齐、评估和安全
问题已经解决。

## 2. 当前实验基线

当前 Windows 虚拟机暴露 4 个物理核、8 个逻辑处理器，CPU 为 2.5 GHz x86-64，
可用 AVX2/FMA，无可用 RDMA adapter，也没有 GPU。

`wbox-hpc-lab` 已建立以下可复现实验：

```text
CPU 执行
├── scalar integer oracle
├── 显式 AVX2 kernel
├── borrowed-shared 多线程
├── AVX2 × 多线程
└── Windows named shared mapping 多进程

数据路径
├── 同一映射直接初始化和读取
├── 初始化后 application-level logical copies = 0
└── 每进程结果使用宿主探测的独立 cache-line slot（本机 64 bytes）
```

整数混合基准在 4,000,000 项、32 rounds、repeat=3 时取得：

- 单线程 AVX2：相对 scalar `3.89x`；
- 4-thread AVX2：`13.55x`；
- 8-thread AVX2：`10.34x`；
- 8-process shared mapping：`3.02x`，包含进程启动成本。

FP64 AVX2 FMA 长测使用 200,000,000 iterations/worker、repeat=5：

| workers | measured FP64 GFLOPS |
|---:|---:|
| 1 | 37.37-38.82 |
| 2 | 70.41-70.47 |
| 4 | 115.28-115.30 |
| 8 | 143.53-144.51 |

名义峰值粗算为 `4 cores × 2.5 GHz × 16 FP64 FLOP/cycle = 160 GFLOPS`；
FMA 微基准约达到 90%。该值是寄存器密集型峰值取证，不是实际 workload SLA。

AArch64 路径使用 16 条独立 128-bit NEON FP64 FMA 链，按
`16 instructions × 2 lanes × 2 operations = 64 FLOP/iteration` 与 x86-64 保持
相同计数口径。它已通过 `aarch64-apple-darwin` 严格交叉编译，但当前 Windows
虚拟机不能提供 Apple Silicon 运行证据；不得从 x86 数值外推 AArch64 GFLOPS。

复现入口：

```powershell
cargo run --release -p wbox-hpc-lab -- bench --items 4000000 --rounds 32 --repeat 3
cargo run --release -p wbox-hpc-lab -- flops --iterations 200000000 --repeat 5
cargo run --release -p wbox-hpc-lab -- memory --mib 128 --passes 3 --repeat 3
cargo run -p wbox-machine --bin wbox-machine-lab -- parallel
```

同一 Windows VM 的 128 MiB 数据集（256 MiB shared mapping，3 passes，median=3）
实测 cold page touch 为约 2293 ns/page，warm 为约 28.9 ns/page；顺序读约
5.82 GiB/s，顺序写约 4.27 GiB/s，copy 的 payload 约 5.08 GiB/s、按读写总流量
约 10.15 GiB/s。数据集约为 35.75 MiB L3 的 3.6 倍，因此不把结果冒充 cache-only
带宽；页触碰差异也只说明当前 VM 的首次提交/缺页成本，不等同于裸机 DRAM 延迟。

## 3. 已获得的方法经验

```text
可信测量
├── 整数 workload 不能换算成 FLOPS
├── FMA 每 lane 按 multiply + add = 2 FLOP
├── kernel 的指令数、lane 数和 worker 数必须可审计
├── 使用独立寄存器依赖链测吞吐，避免只测单链 latency
├── 输出结果必须被观察，防止 dead-code elimination
├── 多次采样并报告 median
└── 理论峰值、微基准和应用吞吐分开报告

并行与内存
├── SIMD、线程、进程不是互斥路线，可以组合
├── worker 数必须扫描物理核与 SMT 区间
├── 共享内存消除 application copy，不消除 cache/page traffic
├── 多进程测量要说明是否包含 spawn 和 IPC 成本
├── cache-line 必须来自宿主事实并作为父/子进程布局协议的一部分
└── cache hierarchy、NUMA placement 和 memory bandwidth 会决定扩展上限

能力声明
├── API 存在 != 硬件存在
├── 硬件被发现 != 路线可用
├── RDMA 需经过 adapter、enable、registration、peer、transfer 分层取证
└── 当前机器只能预填 RDMA 契约，不能宣称实测支持
```

首轮 FMA kernel 曾因把累加器保存在数组循环中只得到约 20 GFLOPS。改为 8 条显式
寄存器链后达到约 144 GFLOPS。经验是：高性能实验必须检查代码生成假设，测试通过
只能证明结果可用，不能证明测到了目标硬件能力。

## 4. 演进路线

```text
AGI-COMPUTE
├── A. 单机真实性
│   ├── FP32/BF16/INT8 可审计 kernel
│   ├── memory bandwidth、latency、cache 与 page-fault lab
│   ├── 简化 Roofline：算术强度 -> compute-bound / memory-bound
│   └── NUMA topology、affinity、huge page 与调度噪声
├── B. 数据移动
│   ├── bounded shared-memory ring 与 backpressure
│   ├── scatter/gather、vectored I/O 与 IOCP
│   ├── zero-copy 生命周期、ownership 和 crash recovery
│   └── RDMA capability/registration/transfer 门禁
├── C. 异构计算
│   ├── CPU/GPU/NPU/LPU 能力和精度矩阵
│   ├── host/device memory、queue、event 与 synchronization
│   ├── kernel placement 与 fallback
│   └── FLOPS/W、FLOPS/USD 与有效利用率
├── D. 分布式执行
│   ├── 节点发现、能力描述和资源租约
│   ├── data/tensor/pipeline/task-graph parallelism
│   ├── checkpoint、重试、幂等和故障域
│   └── topology-aware placement 与跨节点观测
└── E. AGI 实验面
    ├── 可复现环境、数据谱系和确定性边界
    ├── 训练/推理 workload replay 与成本账本
    ├── 小规模实验到 PFLOPS 集群的同构接口
    └── 达到成本窗口后再扩大真实模型实验
```

## 5. wbox-machine 的职责

`wbox-machine` 不负责宣称某种 AGI 算法成立。它负责提供可验证的基础设施接口：

- ISA、精度、拓扑、内存和互连能力；
- execution × data-path 的预填矩阵；
- first-party Rust provider 的选择和结构化 unsupported；
- 测量证据、能力状态和稳定 TODO 标识；
- 单机、异构设备和分布式 fabric 的共同资源模型。

成熟且脱离 wbox 产品语义仍成立的宿主探测、进程、文件、设备访问能力，可反馈并
下沉到 `agenterm-platform`；wbox 继续持有计算路线、guest ABI 和产品验收语义。
当前第一批处理器事实已经按此边界落地：上游零依赖报告 architecture、pointer width、
逻辑处理器数和 CPU feature，`wbox-machine` 负责将其解释为计算、guest 与加速路线；
FMA feature 可用于选择实验内核，但不能替代吞吐计量或加速后端可用性门禁。
`wbox-hpc-lab` 已删除自身的直接 feature detector，以单次共享快照选择 AVX2/FMA/
NEON 内核；这保证同一进程的能力描述、CLI 输出和实验门禁使用同一事实来源。

## 6. 阶段判据

每一阶段都必须同时回答：

1. 测量的是哪种精度、哪类操作、什么数据路径？
2. 理论峰值、实测峰值和 workload 有效吞吐分别是多少？
3. 每美元和每瓦获得多少有效计算，而非标称计算？
4. 瓶颈在计算、内存、互连、调度还是故障恢复？
5. 结果能否由固定命令、固定计数公式和机器可读输出复现？

只有满足这些判据，实验结果才进入长期基线；探索性数字不能升级为产品能力。
