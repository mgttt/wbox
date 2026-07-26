# RustCmd product requirements

Status: active development  
Platform: Windows  
Primary shell: `cmd.exe`

## Product statement

RustCmd is an enhanced, locally scriptable Windows terminal. It keeps the
familiar command-shell workflow while adding left-side tabs, durable exited
tabs, per-tab external composition, and a tmux/RMUX-inspired control plane.

The product must be usable in two equal ways:

1. A person interacts with the native window.
2. A program or agent observes and controls the same window through the CLI.

## Core experience

### Tabs

- Tabs appear vertically on the left.
- A new tab starts `cmd.exe` unless a command is explicitly supplied.
- Each tab has a stable ID, numeric index, editable name, and one terminal pane.
- Selecting a tab changes the visible terminal and composer.
- Closing a live tab terminates its PTY process tree.

### Remain on exit

- Process exit never removes its tab automatically.
- The last terminal screen and exit code remain visible.
- Only an explicit user or CLI close operation removes the tab.

### Per-tab composer

- Every tab stores its own draft independently.
- The composer is outside the terminal grid.
- Send writes the draft to the target PTY, submits Enter, and clears the draft.
- CLI clients can read, replace, and submit the draft.

### Scriptability

- Common tmux command names and format tokens are preferred where semantics
  match.
- RustCmd-only GUI operations use clear extension commands.
- Commands target a tab by index, stable ID, or name.
- Machine consumers get structured JSON through `inspect`/`pane-snapshot`.
- Tests can wait for output or process exit without fixed sleeps.

### Visual observability

- The whole RustCmd window can be saved as PNG.
- A selected tab's terminal viewport can be saved as PNG.
- Screenshot commands return the output path on success.

## Acceptance criteria for the first usable release

- [x] Native window opens without GPU/OpenGL requirements.
- [x] Default tab runs the real system `cmd.exe`.
- [x] Multiple left-side tabs can be created and selected.
- [x] Terminal input and output work through native ConPTY.
- [x] Exited tabs remain until explicitly closed.
- [x] Per-tab composer works from GUI and CLI.
- [x] CLI can return the active tab as stable `id:name`.
- [x] CLI can capture terminal text and inspect tab state as JSON.
- [x] Whole-window and per-pane screenshots produce valid PNG files.
- [x] CLI can wait for expected output or a dead pane.
- [x] Core public-interface feedback loop is automated by `tests/cli_smoke.ps1`.
- [x] Idle terminal rendering is dirty-state driven rather than timer-redrawn.
- [x] Changed frames use GDI double buffering and one final `BitBlt`.
- [x] Raw escaped output, styled cell dumps, and synthetic terminal mouse input
  are exposed for rendering diagnostics.
- [x] RMUX Byobu F3/F4 changes update the active marker immediately.
- [x] RMUX Byobu status labels can be clicked in RustCmd to activate a window,
  including the RMUX 0.9.1 Windows mouse-input compatibility bridge.
- [x] New ConPTY tabs inherit the current terminal grid size so Windows RMUX
  places its status rows against the bottom edge on first attach.
- [x] Minimizing the GUI does not shrink active ConPTY tabs to the iconic
  window rectangle.
- [ ] Keyboard, mouse, resize, ANSI color, CJK, and long-output behavior have
  repeatable regression coverage.
- [ ] High-throughput terminal rendering is visually verified with no flicker.
- [ ] Installer/update and a stable location on `PATH` are defined.

## Compatibility policy

Compatibility is semantic, not a claim that RustCmd already implements every
tmux command. Supported commands must behave consistently; unsupported
operations return an explicit error. One tab currently maps to one pane, so
`split-window` remains a planned feature.

## Product roadmap

### Near term

- Add automated CLI/visual smoke tests.
- Expand RMUX-in-RustCmd rendering, function-key, and status-click regression
  tests beyond the current feedback-driven coverage.
- Add `stream-pane` output subscriptions.
- Add `set-composer --stdin` and file input for lossless multiline drafts.
- Add configurable shell, font, colors, working directory, and startup tabs.
- Improve Unicode/CJK width and font rendering.

### Multiplexer depth

- Split panes and layout commands.
- Session persistence/restore.
- Named-pipe IPC with protocol versioning.
- Command aliases and a larger tmux/RMUX compatibility matrix.
- Broadcast input and synchronized panes.

### Agent-grade automation

- Structured terminal cell snapshots.
- Output sequence numbers and incremental reads.
- Stable event stream for output, title, activity, and exit.
- Declarative assertions such as text, regex, quiet period, and exit code.
- Optional local SDK built on the same versioned protocol.

## Non-goals for the current milestone

- Replacing PowerShell or cmd language semantics.
- Remote terminal hosting.
- Full tmux compatibility before the single-pane tab experience is stable.
- Automatically deleting or hiding exited tabs.
