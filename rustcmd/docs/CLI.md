# RustCmd CLI

The executable is both the GUI entry point and the local control client:

```powershell
$r = ".\target\release\rustcmd.exe"
```

Targets accepted by `-t` are a numeric index, a stable `@id`, or a tab name.

## tmux-style commands

```text
new-session [-s name]
new-window [-d] [-n name] [command [args...]]
list-sessions
has-session [-t name]
list-windows [-F format]
list-panes [-a] [-t target] [-F format]
select-window -t target
next-window
previous-window
rename-window [-t target] name
rename-session name
send-keys [-t target] [-l] key...
capture-pane -p [-t target]
display-message -p [-t target] format
show-options
kill-window [-t target]
kill-session
kill-server
```

Useful format variables include:

```text
#{session_name} #{window_id} #{window_index} #{window_name}
#{window_active} #{pane_id} #{pane_pid} #{pane_dead}
#{window_note} #{terminal_title}
#{pane_current_command} #{pane_width} #{pane_height}
#{pane_input_bytes} #{pane_output_bytes} #{pane_error}
```

The native tmux-style way to read the active tab is:

```powershell
& $r display-message -p '#{window_id}:#{window_name}'
```

RustCmd also provides the shorter equivalent:

```powershell
& $r active-window
```

## RustCmd extensions

### Composer

```powershell
& $r set-composer -t build "cargo test"
Get-Content .\command.txt -Raw | & $r set-composer -t build --stdin
& $r set-composer -t build --file .\command.txt
& $r show-composer -t build
& $r send-composer -t build
```

`--stdin` and `--file` preserve multiline content without shell argument
joining. `send-composer` submits Enter and clears the stored draft.

### Two-line tab metadata

```powershell
& $r rename-window -t build "build"
& $r set-tab-note -t build "核心服务 · 发布前检查"
& $r show-tab-note -t build
```

The first line combines the main process with the latest terminal OSC TITLE.
`rename-window` / `new-window -n` controls the RustCmd name used for targeting
and as the primary fallback title. The second line is a user note; terminal
TITLE updates never overwrite it. Right-clicking a sidebar tab opens the same
note editor in the composer area.

### Settings

```powershell
& $r get-settings
& $r set-setting terminal.font-family "Sarasa Fixed SC"
& $r set-setting terminal.font-size 12
& $r ui-action open-settings
```

Settings persist in `%LOCALAPPDATA%\RustCmd\settings.json`. `get-settings`
reports both the requested and Windows-resolved face. See
[FONTS.md](FONTS.md) for CJK width and licensing details.

### Semantic UI automation

```powershell
& $r ui-snapshot
& $r ui-action select-tab -t '@2'
& $r focus composer -t '@2'
& $r wait-ui --active '@2' --focus composer
& $r ui-action close-tab -t '@2'
& $r ui-action cancel
& $r protocol-info
```

`ui-snapshot` reports window/layout geometry, focus, stable tab IDs, running /
dead / error state, draft indicators, feedback, and a pending modal.
`ui-action close-tab` models the GUI safety rule: a live tab produces a
confirmation modal, while a dead tab closes immediately. The tmux-compatible
`kill-window` remains an explicit immediate process-termination command.

`wait-ui` currently supports `--active`, `--focus`, and
`-t target --tab-state running|dead|error`. Timeouts include the last structured
UI state.

### Screenshots

```powershell
& $r screenshot -o rustcmd.png
& $r screenshot-pane -t build -o build-pane.png
```

`screenshot` captures the native window. `screenshot-pane` captures only the
target terminal viewport.

### Rendering diagnostics and mouse input

```powershell
& $r capture-pane --raw-escaped -t build
& $r dump-cells -t build
& $r dump-cells -t build -r 39
& $r send-mouse -t build -x 10 -y 5 --button left
& $r send-mouse -t build -x 10 -y 5 --button left --protocol native
& $r send-mouse -t build -x 10 -y 5 --button left --action press --protocol sgr
& $r send-mouse -t build -x 10 -y 5 --button left --action release --protocol sgr
```

`auto`（默认）会优先兼容 Windows RMUX。若点击行能识别出
RMUX Byobu 的 `N:name` 与 `[N:name]`，则会通过已验证的 F3/F4 路径
完成切换；这是当前 RMUX Windows attach 客户端不消费鼠标输入时的兼容桥。
其余位置尝试 Win32 控制台鼠标记录，失败时回退到 xterm SGR。`native`
强制 Win32 路径，`sgr` 强制转义序列路径。GUI 中的终端左键点击使用相同逻辑。

开发或并行测试实例可设置 `RUSTCMD_IPC_ADDRESS=127.0.0.1:端口`。GUI
及其 CLI 客户端必须使用同一个值；未设置时仍使用按 Windows 用户名派生的
默认本机端口。

`dump-cells` returns styled/non-empty cells as JSON. Mouse coordinates are
zero-based terminal cells and use xterm SGR mouse encoding.

### Structured state

```powershell
& $r inspect
& $r pane-snapshot -t build
```

Both return JSON. A window record includes identity, process state, terminal
size, byte counters, composer content, error state, and captured terminal text.

### Deterministic waits

```powershell
& $r wait-pane -t build --contains "Finished" --timeout-ms 30000
& $r wait-pane -t build --dead --timeout-ms 5000
& $r expect-pane -t build "test result: ok"
```

Exit code is zero when the condition is met and nonzero on timeout or error.

## Key names

`send-keys` recognizes Enter, Escape, Space, Backspace, Tab, arrows, Home, End,
Delete, PageUp, PageDown, F1–F12, and `C-a` through `C-z`. Other values are sent
as text.

## Current compatibility limitation

`split-window` reports an explicit not-implemented error. RustCmd currently maps
one ConPTY pane to each tab.
