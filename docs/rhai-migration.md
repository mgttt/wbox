# Rhai automation migration

Wbox is migrating automation incrementally onto Agenterm's unrestricted local
Script Runtime. Existing PowerShell entry points remain supported until their
Rhai replacements have equivalent behavior and regression evidence.

## Current slice

- `scripts/rhai/lint.rhai`, `build.rhai`, `check.rhai`, and
  `check-portable-targets.rhai` are native replacement implementations.
- Native/network/Ubuntu/cleanup probes and Rhai implementations for the
  WP.1–WP.27 product journeys are available through
  `test-windows-product.rhai`.
- Their `.ps1` files are compatibility wrappers that invoke the runtime.
- `C:\Users\wjc2022\bin\rhai.cmd` resolves the local
  `agenterm-script.exe` (override with `AGENTERM_SCRIPT_EXE`).

Run either:

```powershell
rhai.cmd run scripts/rhai/lint.rhai -- Static
.\scripts\lint.ps1 -Mode Static
```

The wrapper passes the repository as both the runtime working directory and
project root, so callers may invoke it from another directory.

These slices intentionally have no PowerShell subprocess. Remaining parity
gaps are target cleanup LRU/compaction semantics and real Windows-host
execution evidence for the process-tree journeys. `test-windows-product.ps1`
remains as a compatibility wrapper and no longer contains product logic.

Where a product journey explicitly tests a Windows guest workload, the fixture
may still launch `powershell.exe` inside wbox. That is the behavior under test,
not a dependency of the host automation layer; the host runner, orchestration,
filesystem, and assertion logic are all provided by the Rhai runtime.

## Shebang

Rhai source now accepts a conventional first-line `#!...` shebang. Agenterm
normalizes only that prefix into a Rhai comment, preserving the line count and
leaving all other source untouched. This makes `.rhai` files directly usable by
Unix launchers while Windows continues to use `rhai.cmd`.

## Next replacements

Keep each `.ps1` wrapper until parity tests cover exit codes, cleanup
guarantees, and Windows-target behavior; archive the wrapper only after callers
and CI have switched to the Rhai task.
