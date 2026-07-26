# RustCmd capability tree

Status values: **supported**, **partial**, **planned**, **not applicable**.

This is the ownership map for the command surface. A tmux/RMUX name is used
only when its observable meaning matches; RustCmd-only product behavior uses an
explicit extension name.

```text
RustCmd
├─ tmux/RMUX-compatible semantics
│  ├─ server/session
│  │  ├─ new-session, list-sessions, has-session       [supported]
│  │  ├─ rename-session                                [supported]
│  │  └─ kill-session, kill-server                     [partial: one local server]
│  ├─ windows → RustCmd tabs
│  │  ├─ new/list/select/next/previous-window          [supported]
│  │  ├─ rename-window                                 [supported]
│  │  └─ kill-window                                   [supported, explicit immediate kill]
│  ├─ panes → one pane per tab
│  │  ├─ list-panes, capture-pane, send-keys           [supported]
│  │  └─ split-window and layouts                      [planned]
│  ├─ formats/query
│  │  ├─ display-message, list-commands, show-options  [supported]
│  │  └─ documented session/window/pane tokens         [supported subset]
│  └─ terminal compatibility
│     ├─ ConPTY, VT rendering/input, resize             [supported]
│     ├─ F1–F12 including Byobu F2/F3/F4/F6/F8         [supported]
│     └─ RMUX 0.9.1 Windows status-click bridge         [partial compatibility]
└─ RustCmd extensions
   ├─ composition
   │  ├─ show/set/send-composer                         [supported]
   │  └─ set-composer text/--stdin/--file               [supported]
   ├─ observation
   │  ├─ inspect, pane-snapshot, active-window          [supported]
   │  ├─ screenshot, screenshot-pane                    [supported]
   │  └─ dump-cells, raw escaped capture                [supported]
   ├─ semantic UI control
   │  ├─ ui-snapshot, protocol-info                     [supported]
   │  ├─ ui-action, focus, wait-ui                      [supported P0 subset]
   │  └─ hit-test, physical key injection               [planned]
   ├─ safety and durable state
   │  ├─ remain-on-exit                                 [supported]
   │  ├─ GUI live-close confirmation                    [supported]
   │  └─ persisted sessions/drafts                      [planned]
   └─ deterministic terminal control
      ├─ wait-pane, expect-pane, send-mouse             [supported]
      ├─ sequence-numbered incremental output           [planned]
      └─ event stream                                   [planned]
```

## Important semantic boundaries

- The left sidebar owns outer RustCmd tabs. RMUX window labels remain terminal
  content and are never duplicated as RustCmd tabs.
- `ui-action close-tab` follows human GUI safety and confirms a live-process
  termination. `kill-window` is the explicit tmux/RMUX-compatible immediate
  action for scripts.
- Read/query commands do not change the active tab. Target screenshot capture
  currently restores the previous state after rendering and is classified as
  supported with a visual-transient limitation.
- Stable `@id` is the automation identity. Numeric indices may be reused after
  a tab closes.

## Canonical verification

- `tests/cli_smoke.ps1`: PTY, composer, screenshots, exit durability.
- `tests/ux_smoke.ps1`: stable IDs, semantic UI state/actions, lossless
  composer input, live-close cancel, dead-close, protocol discovery.
- `docs/RMUX_COMPAT.md`: nested RMUX rendering, keys, mouse bridge, geometry.
