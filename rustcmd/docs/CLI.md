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
& $r show-composer -t build
& $r send-composer -t build
```

`send-composer` submits Enter and clears the stored draft.

### Screenshots

```powershell
& $r screenshot -o rustcmd.png
& $r screenshot-pane -t build -o build-pane.png
```

`screenshot` captures the native window. `screenshot-pane` captures only the
target terminal viewport.

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

