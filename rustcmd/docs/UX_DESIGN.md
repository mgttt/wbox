# RustCmd UX design

Status: implementation-ready proposal  
Baseline reviewed: 2026-07-26  
Scope: native RustCmd window, RMUX/Byobu use, and CLI-driven feedback testing

## Implementation status

Implemented in the first UX vertical slice:

- clearer session header, labelled New control, tab draft/done/exit states, and
  sidebar summary;
- contextual composer target and shortcut hints without dynamically changing
  PTY geometry;
- live-tab close confirmation, dead-tab immediate close, exit banner;
- Ctrl+Shift+T/W/I, Ctrl+Tab/Ctrl+Shift+Tab, composer Ctrl+Enter/Escape while
  preserving unmodified function keys;
- stable `@id` targeting, `ui-snapshot`, P0 `ui-action`, `focus`, `wait-ui`,
  `protocol-info`, and lossless `set-composer --stdin/--file`;
- public UX integration coverage in `tests/ux_smoke.ps1`.

The compact dynamic composer remains planned until RMUX's Windows attach resize
path can keep its status rows flush after a live ConPTY resize. The current
fixed-height composer is intentionally geometry-stable.

Implemented in the typography/tab-metadata slice:

- branded executable/window icon;
- persistent GUI/CLI terminal font family and point size settings with resolved
  Windows face reporting;
- cell-anchored narrow/wide glyph rendering;
- two-line tabs with program + OSC TITLE on line one and independent user note
  on line two;
- right-click note editing plus `set-tab-note` / `show-tab-note`.

## Product experience model

RustCmd has two nested navigation levels and they must remain visually and
semantically distinct:

```text
RustCmd session
├─ RustCmd tab 0: cmd.exe
├─ RustCmd tab 1: rmux-byobu        ← left sidebar
│  └─ RMUX windows 0..n             ← terminal-owned bottom status
└─ RustCmd tab 2: build [dead]
```

The left sidebar always changes the outer ConPTY/tab. An RMUX status label
always changes an inner RMUX window. RustCmd must not restyle, duplicate, or
claim ownership of the inner RMUX window list.

The product has two equally important operators:

- a person using keyboard, mouse, tabs, terminal, and composer;
- a program or agent using stable IDs, structured state, semantic actions,
  deterministic waits, and screenshots.

Every material GUI state must therefore be both visible to a person and
observable through the CLI. Every primary GUI action must have a semantic CLI
equivalent.

## Interaction principles

1. **The terminal owns ordinary input.** Unmodified characters, navigation
   keys, function keys, and mouse events go to the terminal whenever it has
   focus. This preserves cmd, PowerShell, tmux, RMUX, and TUI behavior.
2. **RustCmd shortcuts use modifiers.** Global application actions must not
   consume F2/F3/F4/F6/F8 or terminal prefix sequences.
3. **Exited output is durable.** Exit changes state, not existence. A tab is
   removed only by an explicit close action.
4. **Danger is proportional to state.** Closing a dead tab is immediate;
   closing a live tab requires confirmation because it terminates a process
   tree.
5. **Selection is not mutation.** Reading state or taking a target screenshot
   must not visibly switch the active tab, move focus, or alter a draft.
6. **Feedback is local and actionable.** Errors appear near the affected
   surface and remain inspectable; they are not silently stored only in memory.
7. **Stable identity beats position.** Automation should retain `@id`; numeric
   indices and names remain convenient human selectors.

## Target window layout

```text
┌─ RustCmd / session-name ────────┬─ active tab name · process state ────────┐
│ [+ New tab]                     │                                          │
│ ● 0  cmd.exe                    │                                          │
│ ● 1  rmux-byobu                 │            terminal viewport             │
│ ◉ 2  build                 ×    │                                          │
│ ! 3  tests [exit 1]        ×    │                                          │
│                                │                                          │
│                                ├─ contextual exit/error banner (when any) ─┤
│                                ├─ composer draft (collapsed when empty) ─┬─┤
│ session/process summary         │ multiline external input               │▶│
└─────────────────────────────────┴──────────────────────────────────────────┴─┘
```

Layout rules:

- Sidebar default width: 200–220 device-independent pixels. It is resizable
  between 150 and 360 px and remembers its width.
- The sidebar header shows the session name and one clearly labelled new-tab
  control. The current lone `+` is too easy to miss.
- The terminal fills all space above the composer. PTY rows/columns are derived
  from the exact terminal rectangle and synchronized after a settled resize.
- Empty composer is collapsed to a 34–40 px single-line affordance. Focusing it
  or entering multiline content expands it to 72–160 px. The divider is
  draggable and the height is remembered.
- With the composer explicitly hidden, the terminal extends to the bottom edge.
  Showing or resizing the composer triggers one PTY resize after layout settles,
  not an intermediate series that can confuse RMUX.
- At small sizes, terminal visibility wins: sidebar may collapse to icons and
  composer height is capped at one third of the client height.
- All dimensions are DPI-aware. No clickable target is smaller than 28×28 px.

## Sidebar and tab specification

Each tab row contains:

```text
state dot | numeric index | elided name | optional draft/activity badge | close
```

State colors are never the only signal:

| State | Visual | Accessible/status text |
|---|---|---|
| running, inactive | green dot | `running` |
| running, active | active background + green ring | `active, running` |
| unseen output | small filled activity badge | `new output` |
| exited 0 | hollow amber dot + `[done]` | `exited 0` |
| exited nonzero | warning icon + `[exit N]` | `exited N` |
| PTY/input error | error icon | concise error |
| non-empty draft | pencil/dot badge | `draft` |

Tab behavior:

- Clicking the body selects the tab and restores terminal focus.
- Close is shown on hover, keyboard focus, or dead/error state. It has a tooltip.
- Closing a live tab opens an in-window confirmation:
  `Close “build” and terminate PID 1234 and its child processes?`
  The destructive button is labelled `Terminate and close`; Escape cancels.
- Closing a dead tab is immediate. The preserved screen is never cleared before
  the row disappears.
- Middle-click follows the same safety rule as close; it is not an unconfirmed
  live-process kill.
- Double-clicking the name enables inline rename. Enter commits, Escape cancels.
- When tabs exceed available height, the list scrolls. The active tab is always
  scrolled into view. Wheel events over the sidebar scroll the sidebar and are
  not sent to the PTY.
- Reusing a vacated numeric index is allowed, but stable `@id` is never reused
  during a server lifetime.
- The sidebar footer shows a compact summary such as `4 tabs · 1 exited`.

## Terminal and RMUX behavior

### Focus and input ownership

- A newly created or selected tab focuses its terminal.
- Clicking terminal cells focuses the terminal before forwarding mouse input.
- Unmodified F1–F12, arrows, Page keys, Escape, and characters are forwarded to
  the PTY. In particular, F2/F3/F4/F6/F8 remain available to Byobu/RMUX.
- RustCmd does not reserve a tmux-style prefix at this milestone.
- Terminal cursor visibility follows the VT state and disappears when the tab is
  dead.

### RMUX status interaction

- RustCmd treats the bottom status rows as terminal content, not application
  chrome.
- Native RMUX mouse input is preferred. The current RMUX 0.9.1 Windows status
  bridge may recognize `N:name`/`[N:name]` ranges and emit equivalent F3/F4
  navigation only when native input is unavailable.
- The bridge must be conservative: a timestamp such as `22:40`, filesystem
  path, or ordinary terminal line must never be recognized as a window label.
- Status clicks must have no side effect outside the target RMUX client.
- The active marker must update in the next rendered RMUX frame. RustCmd should
  not synthesize its own highlight.
- PTY size equals the current terminal grid, including after first attach,
  restore from minimize, composer expand/collapse, and DPI/monitor changes.

### Selection, copy, and scrolling

These are P1 because mouse passthrough and local text selection need an explicit
mode boundary:

- Shift+drag performs local text selection; plain drag is forwarded when the
  terminal has mouse reporting enabled.
- Ctrl+Shift+C copies a local selection; Ctrl+Shift+V pastes to the PTY.
- Wheel scrolls local history when the child has not requested mouse reporting;
  otherwise it is forwarded. Shift+wheel always requests local history.
- While viewing history, a visible `↓ Live` affordance returns to the live
  screen. New output does not forcibly discard the user's reading position.

## Composer specification

The composer is a per-tab draft editor outside the terminal. It is intended for
long commands, multiline pastes, and agent-authored input—not as a replacement
for normal terminal typing.

- Placeholder: `Compose input for 1:rmux-byobu`.
- `Ctrl+Shift+I` focuses and expands the composer.
- `Ctrl+Enter` sends the entire draft followed by Enter.
- `Shift+Enter` and Enter insert a newline while the multiline editor is
  focused. The Send button remains the obvious mouse path.
- Escape returns focus to the terminal without discarding the draft.
- A successful send clears the draft and shows a brief `Sent to @id` result.
- Sending is disabled for a dead tab with the explanation
  `Process exited; draft preserved`.
- Switching tabs saves the exact draft, selection, and scroll position.
- Sending to an inactive tab through CLI does not activate that tab.
- Multiline content is transmitted losslessly. CLI supports `--stdin` and
  `--file`; shell argument joining is not the canonical multiline path.
- Optional P1 behavior: a small drop-down chooses `Send + Enter` or `Send raw`.
  The selected mode is stored per tab.

Drafts are runtime state in P0. Persisting them across server restarts is P2 and
must be opt-in because drafts may contain secrets.

## Keyboard map

RustCmd application shortcuts:

| Shortcut | Action |
|---|---|
| Ctrl+Shift+T | new RustCmd tab |
| Ctrl+Tab / Ctrl+Shift+Tab | next / previous RustCmd tab |
| Alt+1…9 | select RustCmd tab index 0…8 |
| Ctrl+Shift+W | close current tab, confirming if live |
| Ctrl+Shift+R | rename current RustCmd tab |
| Ctrl+Shift+I | focus/expand composer |
| Ctrl+Enter (composer focused) | submit composer |
| Escape (composer/modal focused) | return/cancel to terminal |
| Ctrl+Shift+C / Ctrl+Shift+V | copy selection / paste |

The shortcut layer must inspect modifiers, not only `WM_KEYDOWN`. A CLI query
must expose the effective bindings so tests and future configuration do not
hard-code assumptions.

## Feedback, errors, and empty states

- `last_error` must be rendered, timestamped, scoped (`app`, `tab`, `pty`,
  `composer`, or `ipc`), and available through structured inspection.
- Recoverable failures show a non-modal banner above the composer. It contains a
  concise message, optional details, and dismiss action.
- Fatal startup failure may continue to use a native dialog.
- Input/write errors mark the affected tab and do not masquerade as clean exit.
- Empty session copy is `No tabs. Create a cmd.exe tab` with a visible button.
- Long-running actions expose `pending` state; duplicate activation is disabled.
- Destructive confirmation and errors must remain usable from keyboard.
- Success toasts are reserved for otherwise invisible actions, such as an
  external composer submit. Routine tab selection does not show a toast.

## Compatibility and extension capability tree

This tree is the documentation and implementation ownership model. Each leaf
must appear in the compatibility matrix with one state:
`supported`, `partial`, `planned`, or `not applicable`.

```text
RustCmd command surface
├─ tmux/RMUX-compatible semantics
│  ├─ server/session
│  │  ├─ new-session, list-sessions, has-session
│  │  ├─ rename-session
│  │  └─ kill-session, kill-server
│  ├─ windows (mapped to RustCmd tabs)
│  │  ├─ new-window, list-windows, select-window
│  │  ├─ next-window, previous-window, rename-window
│  │  └─ kill-window
│  ├─ panes (one pane per tab today)
│  │  ├─ list-panes, capture-pane, send-keys
│  │  └─ split-window / layouts [planned]
│  ├─ formats and query
│  │  ├─ display-message, list-commands, show-options
│  │  └─ supported #{session_*}, #{window_*}, #{pane_*} tokens
│  └─ terminal compatibility
│     ├─ ConPTY + VT screen/input/resize
│     ├─ function keys and ANSI rendering
│     └─ RMUX Windows status mouse compatibility bridge
└─ RustCmd extensions
   ├─ composition
   │  ├─ show-composer, set-composer, send-composer
   │  └─ stdin/file input modes [supported]; raw mode [planned]
   ├─ observation
   │  ├─ inspect, pane-snapshot, active-window
   │  ├─ screenshot, screenshot-pane
   │  └─ dump-cells, raw escaped capture
   ├─ deterministic control
   │  ├─ wait-pane, expect-pane, send-mouse
   │  ├─ semantic UI action and focus [supported P0 subset]
   │  └─ semantic resize [planned]
   └─ event automation
      ├─ sequence-numbered output reads [planned]
      ├─ wait-ui [supported P0 subset]
      ├─ event stream [planned]
      └─ protocol/version/capability discovery [supported P0]
```

Compatibility naming rule: use the tmux/RMUX name only when observable
semantics match. A RustCmd-specific operation gets an explicit extension name;
it must not silently weaken a familiar tmux command.

## CLI additions needed for autonomous UX testing

### P0 control and observation

```text
ui-snapshot [--pretty]
ui-action <action> [--target @id] [action options]
focus <terminal|composer|sidebar> [-t target]
resize-window --width px --height px
resize-pane [-t target] -x cols -y rows
wait-ui <condition> [--timeout-ms N]
set-composer [-t target] (--stdin | --file path | text)
screenshot-pane [-t target] -o path --no-activate
protocol-info
```

`ui-snapshot` returns at least:

```json
{
  "protocol_version": 1,
  "window": {
    "client_width": 1180,
    "client_height": 760,
    "dpi": 96,
    "minimized": false,
    "foreground": true
  },
  "layout": {
    "sidebar": {"x": 0, "y": 0, "width": 210, "height": 760},
    "terminal": {"x": 210, "y": 0, "width": 970, "height": 682,
                 "rows": 40, "cols": 119},
    "composer": {"visible": true, "expanded": false, "height": 38}
  },
  "focus": {"surface": "terminal", "window_id": "@2"},
  "tabs": [{
    "id": "@2", "index": 1, "name": "rmux-byobu",
    "active": true, "state": "running", "draft": false,
    "activity": false, "visible": true,
    "bounds": {"x": 6, "y": 94, "width": 198, "height": 33}
  }],
  "modal": null,
  "feedback": []
}
```

`ui-action` is semantic and stable across DPI/layout changes:

```text
ui-action new-tab
ui-action select-tab -t @2
ui-action close-tab -t @2
ui-action confirm
ui-action cancel
ui-action composer-send -t @2
```

Inline rename and semantic RMUX-window selection remain planned actions; current
RMUX status selection is tested through `send-mouse` terminal-cell coordinates.

This semantic layer is the default for product tests. Pixel/cell injection is a
lower-level diagnostic, not the only automation path.

`wait-ui` conditions initially include:

```text
--focus terminal|composer|sidebar
--active @id
--tab-state running|dead|error
--feedback-kind error|success
--layout-stable-ms N
```

All wait output is structured JSON and timeout errors include the last observed
state.

`protocol-info` reports protocol version, build version, command capabilities,
format tokens, and feature flags. Tests skip a planned capability based on this
response rather than parsing help text.

### P1 diagnostic input and event APIs

```text
hit-test --x px --y px
send-key [-t target] --key F3 [--ctrl] [--shift] [--alt] [--action tap|down|up]
click-ui --x px --y px [--button left] [--action click|down|up]
set-window-state normal|minimized|maximized
read-output [-t target] --after sequence [--max-bytes N]
watch-events [--json-lines] [--types output,exit,active,layout,feedback]
list-bindings [--json]
```

`hit-test` returns the semantic target (`sidebar.tab.close`, `terminal.cell`,
`composer.send`, etc.), target ID, pixel bounds, and terminal cell where
applicable. `send-key` is necessary to verify modifier routing and real F-key
behavior rather than relying only on byte-oriented `send-keys`.

Every output mutation increments a per-pane sequence number. `read-output` and
events use this sequence so a test can distinguish newly produced text from old
screen content.

### P2 visual regression utilities

```text
screenshot --region window|sidebar|terminal|composer|feedback
screenshot --metadata output.json
wait-frame --after frame_sequence
get-theme --json
set-test-theme deterministic
```

Screenshot metadata records build, DPI, pixel bounds, grid size, active ID, and
frame sequence. A deterministic test theme/font makes pixel diffs meaningful;
semantic/cell assertions remain the primary cross-machine checks.

## Prioritized delivery

### Recommended implementation slice for the current Win32/GDI architecture

The next iteration should stay inside `src/main.rs` and the existing loopback
IPC. It does not require a UI framework, renderer replacement, named-pipe
migration, split panes, or persistent settings.

Implement this vertical slice:

```text
visible state
├─ compact composer: 38 px empty, 78 px focused/non-empty
├─ dead/error banner above composer
├─ tab draft badge and explicit [dead]/[exit N] state
└─ close-live confirmation rendered in the client window

keyboard
├─ modifier-aware Ctrl+Shift+T / W / I
├─ Ctrl+Tab and Ctrl+Shift+Tab
├─ Ctrl+Enter / Escape while composer is focused
└─ unchanged forwarding of unmodified F1–F12

automation
├─ ui-snapshot
├─ ui-action new-tab|select-tab|close-tab|confirm|cancel|composer-send
├─ focus terminal|composer|sidebar
├─ wait-ui --active|--focus|--tab-state|--layout-stable-ms
├─ protocol-info
└─ set-composer --stdin|--file
```

Use existing GDI primitives for banners, badges, and the confirmation overlay.
Represent modal, feedback, focus surface, composer expansion, and layout/frame
sequence directly in `AppState`. Reuse existing IPC command dispatch and JSON
serialization. Add pure layout rectangles and hit-test functions so unit tests
can verify them without a visible window.

Defer sidebar resizing, local text selection, event streaming, physical
modifier injection, saved preferences, CJK renderer work, and visual pixel
diffing until after this slice passes its public-interface tests.

The slice is complete only when one integration test performs:

```text
create live tab
→ populate composer
→ observe expanded/draft state
→ submit and wait for output
→ request close and observe confirmation
→ cancel and prove process remains live
→ exit process and observe dead state/banner
→ close dead tab without confirmation
```

### P0 — coherent and safe daily use

1. Render errors and exit state; add live-close confirmation.
2. Implement collapsed/expandable composer with focus and send feedback.
3. Add modifier-aware global shortcuts while preserving raw F keys.
4. Add sidebar scrolling, clearer new-tab affordance, draft/exit indicators,
   and close-on-hover.
5. Make layout DPI-aware and keep PTY geometry synchronized after one settled
   layout change.
6. Add `ui-snapshot`, semantic `ui-action`, `focus`, resize, `wait-ui`,
   `protocol-info`, and lossless composer input.
7. Ensure target pane screenshots and observation commands never activate or
   otherwise mutate the GUI.
8. Convert the capability tree into the canonical command compatibility matrix.

### P1 — terminal quality and power-user flow

1. Local selection, copy/paste, scrollback viewing, and explicit mouse-mode
   boundary.
2. Rename UI, activity/unread badges, resizable sidebar/composer.
3. Physical key/mouse injection, hit testing, effective binding query.
4. Sequence-numbered incremental output and JSON-lines event stream.
5. CJK/wide-cell, combining mark, cursor shape, ANSI attribute, long-output,
   resize, minimize/restore, and RMUX regression suites.
6. Configurable default shell, working directory, font, theme, and startup tabs.

### P2 — persistence and advanced multiplexing

1. Opt-in session/tab/draft restoration with secret-aware defaults.
2. Command palette, searchable scrollback, tab reordering, and profiles.
3. Split panes, layouts, broadcast input, and synchronized panes.
4. Deterministic visual-regression theme and screenshot metadata.
5. Accessibility provider support and full screen-reader names/roles/states.

## Acceptance criteria

### P0 human UX

- F2/F3/F4/F6/F8 reach RMUX unchanged when the terminal is focused.
- Ctrl+Shift+T creates an outer RustCmd tab without affecting the RMUX session.
- A live tab cannot be terminated by a single accidental close click.
- A dead tab remains selectable with its last screen and exit code until close.
- Dead-tab composer draft remains readable and cannot be mistakenly submitted.
- An empty composer occupies no more than 40 px; expanding/collapsing it leaves
  the RMUX status flush with the terminal bottom after resize settles.
- Twenty tabs remain navigable; selecting an off-screen tab reveals it.
- Every PTY/input error visible in `inspect` is also visible in the GUI.
- Screenshot/capture/inspect of an inactive target leaves active tab, focus,
  composer text, and visual frame unchanged.

### P0 agent UX

- A test can discover features, create a tab, focus a surface, resize the
  window, perform a semantic action, wait for stable layout/output/exit, inspect
  exact GUI state, and capture PNG without fixed sleeps.
- UI records use stable `@id`, explicit enums, pixel bounds, and terminal grid
  coordinates; undocumented label parsing is unnecessary.
- Timeouts return nonzero and include the final observed condition/state.
- All mutating commands identify what changed; all read-only commands are
  side-effect free.

### RMUX regression

- Launch/attach at at least three client sizes; each reports the same PTY grid
  as `ui-snapshot`, and all status rows touch the terminal bottom.
- F2 creates an RMUX window; F3/F4 change the active RMUX marker on the next
  frame; F6 detach and F8 rename follow configured Byobu semantics.
- Clicking each visible RMUX window label activates exactly that window.
- Status-click recognition never activates a window when clicking CPU, disk,
  date/time, blank status space, or ordinary terminal text.
- Minimize/restore, composer expand/collapse, and outer-tab switching do not
  kill, detach, or silently resize RMUX to a stale grid.

## Test strategy

Tests should form a three-layer pyramid:

1. **Model/unit:** selector resolution, shortcut matching, hit testing, status
   label parsing, layout math, format rendering, and state transitions.
2. **Public CLI integration:** semantic action → deterministic wait → structured
   assertion, including remain-on-exit and live-close confirmation.
3. **Visual/physical smoke:** screenshots plus real Win32 key/mouse injection at
   representative DPI and sizes. Use cell/style assertions before pixel diffs.

Each end-to-end test creates a uniquely named tab and closes only that tab.
Tests must not kill an existing user session or assume numeric index reuse.
