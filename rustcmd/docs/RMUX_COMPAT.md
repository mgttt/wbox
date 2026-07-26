# RMUX compatibility notes

Status: active investigation  
Last verified: 2026-07-26

## Working

- RMUX can attach and run inside a RustCmd tab backed by `rmux-pty`.
- F2/F3/F4 input reaches RMUX.
- Alternate-screen colors and the three-row status background are parsed and
  rendered.
- RustCmd exposes screenshots, raw escaped output, and styled cell dumps for
  repeatable diagnosis.

## Blank status text investigation

Observed result:

- the status background is visible;
- `dump-cells -r 39` reports every bottom-row cell as `fg=Idx(7)`,
  `bg=Idx(4)`, text `" "`;
- `capture-pane --raw-escaped` contains the blue-background SGR sequence,
  spaces, and erase-line commands, but no status text bytes.

Therefore the missing status text is upstream of RustCmd's parser and GDI
renderer.

The active RMUX server has these options:

```text
status 3
status-format[0] "#(... rmux_byobu_status.ps1 -Row 0 ...)"
status-format[1] "#(... rmux_byobu_status.ps1 -Row 1 ...)"
status-format[2] "#(... rmux_byobu_status.ps1 -Row 2 ...)"
```

`rmux_byobu_status.ps1` synchronously invokes `rmux list-windows`. This creates
a cycle:

```text
RMUX daemon renders status
  -> waits for PowerShell format job
     -> PowerShell calls the same RMUX daemon
        -> waits for daemon response
```

The format job cannot complete normally, so RMUX emits styled blank rows. The
fix belongs in the Byobu status provider: use RMUX-native formats, or read a
cache maintained by an independent updater. A `#()` job must not synchronously
call the same daemon that is waiting for that job.

## Fresh daemon constraint

Starting a second RMUX daemon from inside RustCmd currently fails when RustCmd
itself is enclosed by a Windows Job Object that forbids process breakaway.
Attaching to an RMUX daemon launched outside that job works. This is an RMUX
daemon-lifetime constraint, not a ConPTY rendering failure.

## Regression procedure

```powershell
$r = ".\target\release\rustcmd.exe"
$rmux = "$env:USERPROFILE\rmux\bin\rmux.exe"

& $r new-window -d -n rmux-render-test $rmux
& $r screenshot-pane -t rmux-render-test -o target\rmux-render.png
& $r dump-cells -t rmux-render-test -r 39
& $r capture-pane --raw-escaped -t rmux-render-test
```

Future automated coverage should also:

- send F2/F3/F4/F6/F8 and assert the expected RMUX state transition;
- test resize while attached;
- compare alternate-screen and normal-screen snapshots;
- test SGR mouse press/release when RMUX mouse mode is enabled;
- validate Unicode, wide glyphs, bold/dim/underline, and cursor shapes.

