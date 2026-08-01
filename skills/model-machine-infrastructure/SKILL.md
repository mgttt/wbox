---
name: model-machine-infrastructure
description: Convert hardware, OS, emulator, VM, WASM, accelerator, device, and distributed-runtime research into explicit Rust capability contracts, prefilled matrices, structured TODOs, lab tools, and executable gates. Use when extending wbox-machine, studying projects such as v86/QEMU/Podman, adding ISA or guest ABI support, modeling GPU/NPU/LPU or ESP32-class devices, or designing point-line-plane-fabric infrastructure topology.
---

# Model Machine Infrastructure

Turn research into contracts that distinguish observed facts, product intent, and implemented evidence.

## Establish Ownership

Keep dependencies and responsibilities one-way:

```text
product policy and routing
    -> wbox-machine contracts and matrices
        -> execution providers
            -> agenterm-platform host mechanisms / OS and hardware ABI
```

Keep OCI policy, guest ABI, ISA routes, device models, and provider selection in wbox. Promote only host-generic discovery, process, file, lock, path, and device-access mechanisms to `agenterm-platform`.

## Execute The Workflow

1. Read `PRD.md`, `docs/architecture.md`, the affected crate, and current tests before editing.
2. For an external reference, use primary sources and extract mechanisms, boundaries, tests, limitations, and failure semantics. Do not copy its product decomposition blindly.
3. Label every statement as observed, inferred, target, or unknown. Never convert existence of an API/device into `available`.
4. Choose independent matrix dimensions. Keep desktop OS guests, MCU firmware, accelerators, and deployment environments separate unless an execution route genuinely joins them.
5. Model point/line/plane/fabric when work spans resources:
   - point: resource identity and capability;
   - line: transport, direction, bandwidth/error/isolation contract;
   - plane: scheduling or distribution domain;
   - fabric: cross-domain placement, coordination, consistency, and failure policy.
6. Prefill every intended cell. Mark incomplete cells `planned` or `research`, include a stable `TODO(NAME)`, and explain the missing evidence.
7. Add invariant tests for cardinality, uniqueness, references, and the rule that unprobed work cannot be available.
8. Add or extend a small lab command that prints or checks the contract. Prefer useful inspection over test-only scaffolding.
9. Update the PRD tree and architecture ownership text. Do not duplicate rolling status into secondary docs.
10. Run targeted tests, then the repository gate. Commit only after the worktree and remote-main relationship are understood.

## Use The Lab

Run from the wbox repository root:

```powershell
cargo run -p wbox-machine --bin wbox-machine-lab -- host
cargo run -p wbox-machine --bin wbox-machine-lab -- matrix
cargo run -p wbox-machine --bin wbox-machine-lab -- devices
cargo run -p wbox-machine --bin wbox-machine-lab -- accelerators
cargo run -p wbox-machine --bin wbox-machine-lab -- topology
cargo run -p wbox-machine --bin wbox-machine-lab -- wasm
cargo run -p wbox-machine --bin wbox-machine-lab -- inspect <artifact>
cargo run -p wbox-machine --bin wbox-machine-lab -- check
```

Treat lab output as contract evidence, not full product acceptance. Run host-specific product gates before changing a route to available.

## Guardrails

- Work directly on `main`; do not create a worktree.
- Keep first-party Rust implementation as the target. External products are behavioral references, not runtime providers.
- Separate hardware discovery from usability probes and from product acceptance.
- Do not force a new domain into an existing Cartesian product merely to reuse an enum.
- Do not introduce an abstraction that requires moving CPU/memory hot paths before their behavior is gated.
- Reject unsupported artifacts explicitly instead of guessing guest identity.
- Preserve structured failure reasons and stable TODO identifiers.

Read [references/capability-model.md](references/capability-model.md) when deriving a new matrix, topology, WASM-machine boundary, or lessons from an external emulator.
