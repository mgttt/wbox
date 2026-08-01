# wbox-hpc-lab

`wbox-hpc-lab` turns `wbox-machine` parallel-compute contracts into small,
repeatable host experiments. It is a diagnostic lab, not a production
scheduler.

On Windows the same paging-file-backed named mapping stores the input for all
modes. Threads borrow slices from that mapping. Child processes reopen it by
name and write checksums to separate 64-byte result slots. The benchmark also
contains an explicit AVX2 kernel and composes it with scoped threads.

```powershell
cargo run --release -p wbox-hpc-lab -- bench
cargo run --release -p wbox-hpc-lab -- bench --items 4000000 --rounds 32 --repeat 3
cargo run --release -p wbox-hpc-lab -- flops
cargo run --release -p wbox-hpc-lab -- flops --iterations 200000000 --repeat 5
```

The reported time is the median. Process measurements include process startup.
Every mode must produce the scalar checksum before a result is accepted.

The `flops` command measures compute-bound FP64 AVX2 FMA throughput. One loop
executes eight independent 256-bit FMA instructions. Each instruction has four
FP64 lanes and each fused multiply-add counts as two floating-point operations,
so the audited count is `8 * 4 * 2 = 64 FLOP` per worker iteration. Independent
register chains expose throughput instead of measuring one dependency chain's
latency. This is a best-case arithmetic microbenchmark, not an application
performance promise; memory bandwidth and arithmetic intensity still bound real
workloads.

`logical_copies=0` has a narrow meaning: after initialization, the benchmark
does not copy the dataset between application buffers or processes. It does not
mean zero memory traffic, zero page faults, or that CPU caches remain coherent
without hardware work.

Current experiments establish:

- scalar, explicit AVX2, scoped-thread, and AVX2 plus thread execution;
- named shared-memory execution across Windows processes;
- cache-line-separated result slots to avoid intentional false sharing;
- worker-count scans that expose physical-core, SMT, startup, and scheduler
  effects.

RDMA, NUMA placement, shared-memory rings, scatter/gather I/O, and SIMD child
processes remain matrix entries until they have host probes and behavioral
gates. An RDMA API or installable Windows feature alone is not evidence that a
usable RDMA adapter exists.
