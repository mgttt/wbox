# wbox-hpc-lab

`wbox-hpc-lab` turns `wbox-machine` parallel-compute contracts into small,
repeatable host experiments. It is a diagnostic lab, not a production
scheduler.

On Windows the same paging-file-backed named mapping stores the input for all
modes. Threads borrow slices from that mapping. Child processes reopen it by
name and write checksums to separate cache-line-sized result slots. The line
size comes from the current host cache hierarchy and is passed unchanged to
every child process. The benchmark also
contains an explicit AVX2 kernel and composes it with scoped threads.
CPU features, logical processor count, and cache geometry are read once from the
`wbox-machine` hardware snapshot. The lab does not maintain a second runtime
feature detector: AVX2 checksum requires AVX2, x86-64 FP64 requires AVX2 plus
FMA, and AArch64 FP64 requires NEON.

```powershell
cargo run --release -p wbox-hpc-lab -- bench
cargo run --release -p wbox-hpc-lab -- bench --items 4000000 --rounds 32 --repeat 3
cargo run --release -p wbox-hpc-lab -- flops
cargo run --release -p wbox-hpc-lab -- flops --iterations 200000000 --repeat 5
cargo run --release -p wbox-hpc-lab -- memory
cargo run --release -p wbox-hpc-lab -- memory --mib 128 --passes 3 --repeat 3
```

The reported time is the median. Process measurements include process startup.
Every mode must produce the scalar checksum before a result is accepted.

The `flops` command measures compute-bound FP64 SIMD FMA throughput. On x86-64,
one loop executes eight independent 256-bit AVX2 FMA instructions; on AArch64 it
executes sixteen independent 128-bit NEON FMA instructions. The audited counts
are respectively `8 * 4 * 2` and `16 * 2 * 2`, both 64 FLOP per worker
iteration. Independent register chains expose throughput instead of measuring
one dependency chain's latency. The AArch64 kernel has a cross-compilation gate
but still needs native measurements. This is a best-case arithmetic
microbenchmark, not an application performance promise; memory bandwidth and
arithmetic intensity still bound real workloads.

The `memory` command allocates one paging-file/POSIX-shm-backed mapping split
into equal source and destination regions. It reports cold and warm page-touch
latency, sequential read and write throughput, and copy throughput. Copy counts
one payload byte and a minimum of two logical traffic bytes because memory must
be read and written; this is not a claim about cache write-allocate or physical
bus traffic. Both rates are printed. Full-region write/copy verification runs
outside the timed interval. The default 128 MiB region exceeds the current Windows
host's last-level cache, while the complete mapping occupies 256 MiB. Results
are host observations rather than a hardware or cloud SLA.

After each scalar mode, `memory` scans the process-available worker counts and
partitions the same mapping into disjoint thread-owned ranges. Thread timings
include scoped-thread creation. Read checksums must equal the scalar result;
write and copy keep the same global checksum and full-region verification at
every worker count. This exposes the physical-core saturation point and whether
SMT still adds bandwidth without introducing an affinity policy. Each bandwidth
row reports sample `min_ms`, median `elapsed_ms`, and `max_ms` so scheduler noise
is visible rather than hidden behind one aggregate.

Every timed interval is also bracketed by `agenterm-platform` process metrics.
`faults_total` is available on all hosts; `faults_soft` and `faults_hard` remain
`unknown` unless the native API proves that classification. Metric queries are
outside `elapsed_ms`, but the delta belongs to the whole process interval: it
may include allocator, thread-stack, and runtime faults in addition to dataset
faults. Cold/warm page-touch is therefore the strongest controlled comparison.

`logical_copies=0` has a narrow meaning: after initialization, the benchmark
does not copy the dataset between application buffers or processes. It does not
mean zero memory traffic, zero page faults, or that CPU caches remain coherent
without hardware work.

Current experiments establish:

- scalar, explicit AVX2, scoped-thread, and AVX2 plus thread execution;
- named shared-memory execution across Windows processes;
- cache-line-separated result slots to avoid intentional false sharing;
- cold/warm page-touch and read/write/copy memory-path measurements;
- threaded read/write/copy scaling over process-available CPU counts;
- worker-count scans that expose physical-core, SMT, startup, and scheduler
  effects.

RDMA, NUMA placement, shared-memory rings, scatter/gather I/O, and SIMD child
processes remain matrix entries until they have host probes and behavioral
gates. An RDMA API or installable Windows feature alone is not evidence that a
usable RDMA adapter exists.
