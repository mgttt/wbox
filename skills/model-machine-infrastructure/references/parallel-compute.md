# Parallel Compute Evidence

## Separate The Dimensions

Treat execution and data movement as independent dimensions. SIMD can compose
with threads or processes; a shared mapping can serve scalar or vector kernels.
Prefill their Cartesian product and promote only measured combinations.

Execution examples:

- serial scalar;
- ISA SIMD;
- threads;
- processes;
- SIMD plus threads;
- SIMD plus processes.

Data-path examples:

- private copy;
- borrowed shared memory;
- named shared mapping;
- bounded ring;
- scatter/gather buffers.

## Define Zero Copy Precisely

`zero-copy` must state the boundary. Application-level zero copy means consumers
operate on the same backing allocation after initialization. It does not remove
page faults, cache fills, cache coherence, DMA mapping, or transport traffic.
Report logical copies separately from bytes read and written.

## Build Evidence In Layers

1. Detect ISA, logical processor count, NUMA topology, and adapter capability.
2. Run one scalar oracle and require every optimized checksum to match it.
3. Measure repeat samples and report a robust statistic such as the median.
4. Scan worker counts through the detected hardware limit.
5. Include process startup unless the benchmark explicitly measures steady state.
6. Separate result slots by a cache line before drawing scaling conclusions.
7. Record unsupported hardware as observed absence, not implementation failure.

For RDMA, distinguish API availability, installed OS support, RDMA-capable
adapter discovery, enabled adapter state, peer reachability, registration, and
successful transfer. Only the final stages justify an available product route.

## Interpret Results

- SIMD speedup demonstrates vector-kernel value, not multicore scaling.
- Thread speedup that peaks near physical core count can reveal SMT limits.
- Process results include address-space and startup costs unless amortized.
- A regression at higher worker counts can come from SMT contention, memory
  bandwidth, VM scheduling, NUMA placement, or benchmark noise; collect evidence
  before assigning a cause.
- Shared memory removes application copies but creates synchronization,
  ownership, lifetime, and crash-recovery obligations.

Use `wbox-hpc-lab` for the executable experiment and `wbox-machine-lab parallel`
for the product contract matrix.
