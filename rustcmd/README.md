# RustCmd

RustCmd is a native Windows terminal written in Rust. It launches each
`cmd.exe` inside its own ConPTY and presents tabs in a vertical sidebar.

Core behavior:

- `+` creates another `cmd.exe` tab.
- Tabs are shown on the left.
- `×` is the only operation that removes a tab.
- If `cmd.exe` exits, the terminal output and exit code remain visible.
- Closing a live tab explicitly terminates its child process.

Build and run:

```powershell
cd D:\dev\k3-wbox\rustcmd
cargo run --release
```

Or double-click `build.bat`, then run:

```powershell
.\target\release\rustcmd.exe
```

