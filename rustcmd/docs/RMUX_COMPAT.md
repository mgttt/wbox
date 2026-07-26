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
status-format[0] "#{W:...}"
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

The recursive format job cannot complete normally, so RMUX emits styled blank
rows. The Byobu status provider now reads files maintained by the independent
`rmux_byobu_status_update.ps1` process. RMUX's Windows format jobs displayed
`cmd.exe` output but returned empty output for equivalent `powershell.exe`
jobs, so the live status format uses `cmd.exe /c type` to read the cache. A
`#()` job must not synchronously call the same daemon that is waiting for it.

The fixed screenshot and cell dump show row 0's window list and row 2's
right-aligned CPU, disk, and clock text. Row 1 is intentionally empty while the
session has ten or fewer windows.

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
第一行窗口列表使用 RMUX 原生 `#{W:...}` 循环和 `range=window`，因此
F3/F4 后激活标记会立即更新，且在 `mouse on` 时具备可点击窗口范围。
第二、三行仍由独立状态缓存提供，避免状态脚本递归调用当前 RMUX 守护进程。

Windows 下 RMUX 客户端读取 Win32 控制台鼠标记录，而不是依赖它向宿主开启
xterm SGR 鼠标模式。RustCmd 的 GUI 终端点击和 `send-mouse` 默认会通过
`rmux-pty` 注入原生控制台点击；`--protocol sgr` 可用于明确要求 SGR 的程序。

RMUX 0.9.1 的 Windows attach 客户端在 RustCmd 的 ConPTY 中暂未消费上述
Win32 记录。为保证状态栏交互可用，RustCmd 会识别末三行的 `N:name` 标签及
`[N:name]` 当前标签，并用 F3/F4 完成等价跳转；普通终端区域仍走通用鼠标协议。
