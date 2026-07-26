# RustCmd architecture

## Components

```text
CLI process
  -> loopback JSON IPC
  -> native Win32 GUI/server
       -> tab/session model
       -> vt100 screen parser
       -> rmux-pty
            -> Windows ConPTY
            -> child process + Job Object
```

## Native UI

RustCmd uses Win32 controls and GDI directly. This avoids a GPU/OpenGL runtime
requirement and keeps the binary small. The sidebar and terminal are custom
painted; the composer uses a native multiline Edit control and Button.

Terminal drawing batches adjacent cells with identical colors to avoid
per-character GDI repaint stalls. The 100ms timer only polls state; it invalidates
the window when PTY or command state actually changed. `WM_ERASEBKGND` is
suppressed to avoid clear-then-redraw flicker.

## PTY layer

`rmux-pty` owns PTY allocation, bounded input writes, resize, process waiting,
Windows Job Object teardown, ConPTY flag selection, and native console-key
support. This replaced `portable-pty` after feedback testing exposed a Windows
input-mode mismatch.

Each tab owns:

- a stable RustCmd ID and current numeric index;
- a `PtyMaster` and `PtyChild`;
- a reader thread feeding `vt100::Parser`;
- an exit watcher;
- terminal metadata, byte counters, and a composer draft.

Exit updates tab state but does not remove the tab.

## IPC and command processing

The current control transport is per-user loopback TCP with newline-delimited
JSON request/response envelopes. The GUI is the server and processes requests
on its message loop. The CLI starts the GUI for commands that can create or
attach to a session.

The transport is intentionally behind command DTOs so it can later move to
Windows named pipes and a versioned binary protocol without changing CLI
semantics.

## Feedback testing loop

The intended autonomous loop is:

1. Start RustCmd.
2. Create a uniquely named test tab.
3. Set and submit its composer.
4. `wait-pane` for expected output.
5. Read `pane-snapshot` JSON and `capture-pane` text.
6. Save whole-window and pane PNG screenshots.
7. Exit the child and wait for `pane_dead`.
8. Confirm the tab still exists.
9. Explicitly close the test tab.

This tests the same public interfaces available to users instead of relying on
private test hooks.

## Known design constraints

- One tab currently contains one pane.
- GDI fixed-font rendering needs further CJK/wide-cell work.
- IPC commands that can block must move off the GUI thread as the command set
  grows.
- Screenshot capture currently observes the visible GDI representation.
