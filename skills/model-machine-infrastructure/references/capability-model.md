# Capability Modeling Reference

## Contents

1. Evidence ladder
2. Matrix decomposition
3. Point-line-plane-fabric
4. Learning from v86
5. WASM-machine boundary
6. Promotion checklist

## Evidence Ladder

Use these states consistently:

| State | Meaning |
|---|---|
| declared | The concept exists in the contract; no runtime claim |
| observed | Discovery returned a fact; usability is not established |
| planned | Scope and acceptance path are understood |
| research | The mechanism or acceptance path is unresolved |
| available | Implementation and required target gates passed |
| legacy | A visible migration path exists but violates the target architecture |

An API name, device node, CPU flag, library, or executable proves only discovery. Permissions, firmware, resource ownership, isolation, and a behavioral probe are separate evidence.

## Matrix Decomposition

Prefer several truthful matrices over one meaningless product:

```text
desktop execution: Host OS x Guest OS x primary guest ISA
device execution:  Device family x device ISA x firmware environment
accelerators:      Host OS x accelerator class
deployment:        machine contract x native/WASM/remote provider
topology:          points + links + domains + fabrics
```

Join matrices only through typed identities. For example, an ESP32 RISC-V core is a processor ISA but not automatically a Linux guest route. Likewise, a browser WASM runtime is a deployment environment, not a fourth host OS.

For every matrix, test:

- expected cardinality;
- tuple uniqueness;
- exhaustive enum mapping;
- cross-reference integrity;
- no available state without evidence;
- stable reason/TODO for every incomplete cell.

## Point-Line-Plane-Fabric

### Point

Model identity, kind, architecture, capabilities, state, ownership, and failure scope. Examples: host, CPU, GPU, NPU, LPU, memory, storage, NIC, ESP32.

### Line

Model endpoints, direction, transport, framing, ordering, backpressure, error behavior, security boundary, and state. Examples: shared memory, accelerator interconnect, PCIe, USB/JTAG/UART, network, browser channel.

### Plane

Group points into an execution domain. Model scheduling and distribution such as shared scheduling, pipeline, data parallel, task graph, or message passing. Membership must reference existing points.

### Fabric

Join domains under placement, coordination, consistency, snapshot, migration, and failure-domain policy. Fabric membership must reference existing domains. Keep research fabrics non-available even when individual points are usable.

## Learning From v86

Use [the v86 repository](https://github.com/copy/v86) and [the author's implementation overview](https://gist.github.com/copy/ecc99bac5ca0101e024525ddaf620731) as primary references.

Transfer these methods rather than copying code:

- Keep an interpreter as a correctness baseline and entry/hotness collector.
- Compile hot pages or regions rather than requiring all-or-nothing JIT.
- Adapt guest control flow to WASM structured control flow explicitly.
- Treat paging and memory as a model because WASM does not provide `mmap` semantics.
- Model the whole machine: interrupts, timers, buses, disks, networking, display, input, and virtio-style devices matter as much as the CPU.
- Make snapshots, restore, multi-instance operation, and host communication first-class contracts.
- Publish an honest compatibility table with missing CPU and device behavior.
- Reuse external test corpora as behavioral references while preserving project licensing and first-party implementation rules.

Important v86-specific observations to keep contextual: its current public scope is primarily 32-bit x86; its runtime translation emits WASM for hot code; and it exposes a broad emulated PC device set. These are evidence that depth and integration matter, not a requirement that wbox copy its exact ISA or device choices.

## WASM-Machine Boundary

Treat WASM as a provider/deployment boundary, not an OS guest:

```text
wbox-machine contracts
    -> future wasm-machine provider
        ├── CPU interpreter
        ├── hot-region x-to-WASM translator
        ├── linear-memory address space and paging
        ├── interrupt/timer/device bus
        ├── storage/network/display host adapters
        ├── snapshot/restore
        └── browser/WASI embedding surface
```

Prefill at least these capability cells before implementation: interpreter, dynamic translation, linear memory, device bus, block storage, networking, snapshot, and multi-instance. Keep Browser and WASI embeddings separate where their host APIs differ.

Do not create the implementation crate until the shared contract no longer depends on browser UI policy and at least one deterministic fixture can gate it.

## Promotion Checklist

Promote a mechanism from wbox into `agenterm-platform` only when all are true:

- It is meaningful without OCI, guest ABI, route priority, or wbox naming.
- Its input/output and failure types are host-generic.
- At least two consumers or a clear independent platform need exist.
- The target crate can test it without importing wbox product policy.
- wbox can delete its local copy and directly consume the promoted implementation.

Keep a mechanism in `wbox-machine` when it owns ISA, artifact identity, machine/device topology, guest personality, provider capability, product availability, or distributed execution semantics.
