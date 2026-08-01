# wbox-machine

`wbox-machine` is wbox's infrastructure contract crate. It models host OS,
guest OS, processor ISA, guest ABI, executable format, device/accelerator class,
execution provider, isolation, and the 3 x 3 x 2 product route matrix without
depending on an OS adapter. The broader processor taxonomy also reserves
x86-32, ARM32, RISC-V32, and Xtensa32 outside that desktop route matrix.

The experimental `wbox-machine-lab` binary exercises those contracts without
starting a guest:

```text
cargo run -p wbox-machine --bin wbox-machine-lab -- host
cargo run -p wbox-machine --bin wbox-machine-lab -- matrix
cargo run -p wbox-machine --bin wbox-machine-lab -- devices
cargo run -p wbox-machine --bin wbox-machine-lab -- accelerators
cargo run -p wbox-machine --bin wbox-machine-lab -- topology
cargo run -p wbox-machine --bin wbox-machine-lab -- wasm
cargo run -p wbox-machine --bin wbox-machine-lab -- inspect <executable>
cargo run -p wbox-machine --bin wbox-machine-lab -- check
```

- `host` reports native ISA, logical processors, detected CPU features, and the
  host acceleration API candidate. An API candidate remains `unprobed` until a
  future OS adapter verifies device access, permissions, firmware, and runtime
  usability.
- `matrix` prints every host/guest/ISA route and a status summary.
- `devices` prints the separate ESP32 device matrix. Xtensa32/RISC-V32 and
  bare-metal/FreeRTOS routes are prefilled, but none currently claims availability.
- `accelerators` prints the Windows/Linux/macOS x GPU/NPU/LPU research matrix.
  It defines workload classes without claiming that a driver or runtime is usable.
- `topology` expands the prefilled point/line/plane/fabric graph and validates
  IDs, endpoints, memberships, and cross-layer references.
- `wasm` prints the Browser/WASI x machine-capability research matrix that
  defines the entry conditions for a future independent WASM machine crate.
- `inspect` reads at most 1 MiB of an ELF64, PE32+, or Mach-O64 header, identifies
  its guest/ISA contract, and evaluates the matching route on the current host.
- `check` performs runtime invariants over the prefilled contract matrix.

The processor taxonomy already reserves x86-32, ARM32, RISC-V32, and Xtensa32.
The current inspector still rejects their artifact headers until the executable
and firmware identity model can distinguish OS guests from microcontroller
firmware. It also rejects Mach-O universal files until slice selection exists.
It never executes the inspected file.
