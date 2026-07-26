# RustCmd

RustCmd is a native Windows terminal and scriptable terminal controller written
in Rust. It combines a left-side tab UI, one ConPTY-backed shell per tab, a
per-tab external composer, and a tmux/RMUX-style command line.

## Current highlights

- Native Win32/GDI UI with tabs on the left.
- Branded Windows icon and a persistent terminal font/size settings panel.
- `cmd.exe` is the default shell.
- Two-line tabs separate program/terminal TITLE from a user-maintained note.
- Exited processes leave a `[dead]` tab until the user explicitly closes it.
- Every tab owns a composer text box and Send button.
- Local CLI can create, select, rename, inspect, capture, and drive tabs.
- Whole-window and per-pane PNG screenshots support visual feedback testing.
- PTY process management uses `rmux-pty`.

## Build and run

```powershell
cd D:\dev\k3-wbox\rustcmd
cargo build --release
.\target\release\rustcmd.exe
```

Or double-click `build.bat`.

Run the public-interface smoke test:

```powershell
.\tests\cli_smoke.ps1
```

## Examples

```powershell
$r = ".\target\release\rustcmd.exe"

& $r new-window -d -n build
& $r set-composer -t build "cargo check"
& $r send-composer -t build
& $r wait-pane -t build --contains "Finished" --timeout-ms 30000
& $r capture-pane -p -t build
& $r screenshot-pane -t build -o build.png
```

## Product documentation

- [Product requirements](docs/PRD.md)
- [CLI reference](docs/CLI.md)
- [Architecture](docs/ARCHITECTURE.md)
- [UX design](docs/UX_DESIGN.md)
- [Capability tree](docs/CAPABILITIES.md)
- [Terminal fonts and licensing](docs/FONTS.md)
- [RMUX compatibility notes](docs/RMUX_COMPAT.md)
