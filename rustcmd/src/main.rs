use std::{
    env,
    ffi::c_void,
    io::Write,
    mem,
    ptr,
    sync::mpsc::{self, Receiver, Sender},
    thread,
    time::{Duration, SystemTime},
};

use anyhow::{Context as _, Result};
use rmux_pty::{
    ChildCommand, ProcessId, PtyChild, PtyMaster, TerminalSize,
    write_windows_console_mouse_drag,
};
use serde::{Deserialize, Serialize};
use windows_sys::Win32::{
    Foundation::{COLORREF, HINSTANCE, HWND, LPARAM, LRESULT, RECT, SIZE, WPARAM},
    Graphics::Gdi::{
        BeginPaint, BitBlt, CreateCompatibleBitmap, CreateCompatibleDC, CreateSolidBrush,
        DeleteDC, DeleteObject, DrawTextW, EndPaint, ExtTextOutW, FillRect, FrameRect,
        GetDC, GetDIBits, GetStockObject, GetTextExtentPoint32W, GetTextMetricsW, GetWindowDC,
        InvalidateRect, ReleaseDC, BITMAPINFO, BITMAPINFOHEADER, BI_RGB, DIB_RGB_COLORS,
        PAINTSTRUCT, SelectObject, SetBkColor, SetBkMode, SetTextColor, UpdateWindow,
        DEFAULT_GUI_FONT, DT_END_ELLIPSIS, DT_LEFT, DT_SINGLELINE, DT_VCENTER,
        ETO_OPAQUE, HDC, HFONT, HGDIOBJ, SRCCOPY, SYSTEM_FIXED_FONT, TEXTMETRICW,
        TRANSPARENT,
    },
    System::LibraryLoader::GetModuleHandleW,
    UI::Input::KeyboardAndMouse::SetFocus,
    UI::Shell::ShellExecuteW,
    UI::WindowsAndMessaging::{
        CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GetClientRect,
        GetMessageW, GetWindowLongPtrW, GetWindowRect, GetWindowTextLengthW, GetWindowTextW,
        LoadCursorW, MessageBoxW, MoveWindow, PostMessageW, PostQuitMessage, RegisterClassW,
        SetForegroundWindow, SetTimer, SetWindowLongPtrW, SetWindowTextW, ShowWindow,
        TranslateMessage, CREATESTRUCTW, CS_HREDRAW, CS_VREDRAW, CW_USEDEFAULT,
        ES_AUTOVSCROLL, ES_MULTILINE, ES_WANTRETURN, GWLP_USERDATA, IDC_ARROW,
        MB_ICONERROR, MB_OK, MSG, SW_RESTORE, SW_SHOW, SW_SHOWNORMAL, WM_APP, WM_CHAR, WM_CLOSE,
        WM_COMMAND, WM_CREATE, WM_DESTROY, WM_ERASEBKGND, WM_KEYDOWN, WM_LBUTTONDOWN,
        WM_NCDESTROY, WM_PAINT, WM_SETFOCUS, WM_SIZE, WM_TIMER, WNDCLASSW, WS_BORDER,
        WS_CHILD, WS_CLIPCHILDREN, WS_OVERLAPPEDWINDOW, WS_TABSTOP, WS_VISIBLE,
    },
};

const APP_NAME: &str = "RustCmd";
const INITIAL_ROWS: u16 = 30;
const INITIAL_COLS: u16 = 100;
const SCROLLBACK_LINES: usize = 10_000;
const SIDEBAR_WIDTH: i32 = 210;
const COMPOSER_HEIGHT: i32 = 78;
const TAB_TOP: i32 = 58;
const TAB_HEIGHT: i32 = 36;
const BUTTON_ID: usize = 1001;
const EDIT_ID: usize = 1002;
const TIMER_ID: usize = 1;
const WM_APP_WAKE: u32 = WM_APP + 1;
const IPC_TIMEOUT: Duration = Duration::from_secs(5);
const RAW_OUTPUT_LIMIT: usize = 1024 * 1024;

const COLOR_SIDEBAR: COLORREF = rgb(24, 27, 34);
const COLOR_TERMINAL: COLORREF = rgb(12, 14, 18);
const COLOR_COMPOSER: COLORREF = rgb(31, 35, 44);
const COLOR_TEXT: COLORREF = rgb(214, 220, 230);
const COLOR_MUTED: COLORREF = rgb(145, 153, 168);
const COLOR_ACTIVE: COLORREF = rgb(53, 63, 80);
const COLOR_GREEN: COLORREF = rgb(121, 215, 135);
const COLOR_ORANGE: COLORREF = rgb(245, 190, 100);
const COLOR_RED: COLORREF = rgb(240, 100, 95);

const fn rgb(red: u8, green: u8, blue: u8) -> COLORREF {
    red as u32 | ((green as u32) << 8) | ((blue as u32) << 16)
}

#[derive(Debug, Serialize, Deserialize)]
struct IpcRequest {
    args: Vec<String>,
}

#[derive(Debug, Serialize, Deserialize)]
struct IpcResponse {
    ok: bool,
    output: String,
    error: String,
}

impl IpcResponse {
    fn success(output: impl Into<String>) -> Self {
        Self {
            ok: true,
            output: output.into(),
            error: String::new(),
        }
    }

    fn failure(error: impl Into<String>) -> Self {
        Self {
            ok: false,
            output: String::new(),
            error: error.into(),
        }
    }
}

struct IpcEnvelope {
    request: IpcRequest,
    respond_to: Sender<IpcResponse>,
}

fn main() {
    let arguments: Vec<String> = env::args().skip(1).collect();
    if arguments.first().is_some_and(|arg| arg == "-V" || arg == "--version") {
        println!("rustcmd {}", env!("CARGO_PKG_VERSION"));
        return;
    }
    if arguments.first().is_some_and(|arg| arg == "-h" || arg == "--help") {
        print_help();
        return;
    }
    if !arguments.is_empty() {
        std::process::exit(run_cli(arguments));
    }

    if env::var_os("RUSTCMD_SERVER").is_none()
        && send_ipc_request(vec!["__focus".to_owned()]).is_ok()
    {
        return;
    }

    hide_owned_console_window();
    if let Err(error) = run_gui() {
        let message = wide(&format!("RustCmd failed to start:\n\n{error:#}"));
        unsafe {
            MessageBoxW(ptr::null_mut(), message.as_ptr(), wide(APP_NAME).as_ptr(), MB_OK | MB_ICONERROR);
        }
        eprintln!("{error:#}");
        std::process::exit(1);
    }
}

fn run_gui() -> Result<()> {
    let instance = unsafe { GetModuleHandleW(ptr::null()) };
    if instance.is_null() {
        anyhow::bail!("GetModuleHandleW failed");
    }

    let class_name = wide("RustCmdWindowClass");
    let title = wide(APP_NAME);
    let mut window_class: WNDCLASSW = unsafe { mem::zeroed() };
    window_class.style = CS_HREDRAW | CS_VREDRAW;
    window_class.lpfnWndProc = Some(window_proc);
    window_class.hInstance = instance as HINSTANCE;
    window_class.hCursor = unsafe { LoadCursorW(ptr::null_mut(), IDC_ARROW) };
    window_class.lpszClassName = class_name.as_ptr();
    if unsafe { RegisterClassW(&window_class) } == 0 {
        anyhow::bail!("RegisterClassW failed");
    }

    let window = unsafe {
        CreateWindowExW(
            0,
            class_name.as_ptr(),
            title.as_ptr(),
            WS_OVERLAPPEDWINDOW | WS_CLIPCHILDREN,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            1180,
            760,
            ptr::null_mut(),
            ptr::null_mut(),
            instance,
            ptr::null(),
        )
    };
    if window.is_null() {
        anyhow::bail!("CreateWindowExW failed");
    }

    let edit = unsafe {
        CreateWindowExW(
            0,
            wide("EDIT").as_ptr(),
            wide("").as_ptr(),
            WS_CHILD
                | WS_VISIBLE
                | WS_BORDER
                | WS_TABSTOP
                | ES_MULTILINE as u32
                | ES_AUTOVSCROLL as u32
                | ES_WANTRETURN as u32,
            0,
            0,
            100,
            50,
            window,
            EDIT_ID as *mut c_void,
            instance,
            ptr::null(),
        )
    };
    let send_button = unsafe {
        CreateWindowExW(
            0,
            wide("BUTTON").as_ptr(),
            wide("发送").as_ptr(),
            WS_CHILD | WS_VISIBLE | WS_TABSTOP,
            0,
            0,
            74,
            32,
            window,
            BUTTON_ID as *mut c_void,
            instance,
            ptr::null(),
        )
    };
    if edit.is_null() || send_button.is_null() {
        unsafe { DestroyWindow(window) };
        anyhow::bail!("failed to create composer controls");
    }

    let state = Box::new(AppState::new(window, edit, send_button)?);
    unsafe {
        SetWindowLongPtrW(window, GWLP_USERDATA, Box::into_raw(state) as isize);
        SetTimer(window, TIMER_ID, 100, None);
    }
    if let Some(state) = state_mut(window) {
        state.layout();
        state.load_active_composer();
    }

    unsafe {
        ShowWindow(window, SW_SHOW);
        UpdateWindow(window);
    }

    let mut message: MSG = unsafe { mem::zeroed() };
    while unsafe { GetMessageW(&mut message, ptr::null_mut(), 0, 0) } > 0 {
        unsafe {
            TranslateMessage(&message);
            DispatchMessageW(&message);
        }
    }
    Ok(())
}

unsafe extern "system" fn window_proc(
    window: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match message {
        WM_CREATE => {
            let _ = lparam as *const CREATESTRUCTW;
            0
        }
        WM_TIMER => {
            if let Some(state) = state_mut(window) {
                if state.tick() {
                    unsafe { InvalidateRect(window, ptr::null(), 0) };
                }
            }
            0
        }
        WM_APP_WAKE => {
            if let Some(state) = state_mut(window) {
                if state.tick() {
                    unsafe { InvalidateRect(window, ptr::null(), 0) };
                }
            }
            0
        }
        WM_SIZE => {
            if let Some(state) = state_mut(window) {
                state.layout();
                unsafe { InvalidateRect(window, ptr::null(), 0) };
            }
            0
        }
        WM_PAINT => {
            if let Some(state) = state_mut(window) {
                state.paint();
                0
            } else {
                unsafe { DefWindowProcW(window, message, wparam, lparam) }
            }
        }
        WM_LBUTTONDOWN => {
            let x = (lparam as u32 & 0xffff) as i16 as i32;
            let y = ((lparam as u32 >> 16) & 0xffff) as i16 as i32;
            if let Some(state) = state_mut(window) {
                state.click(x, y);
            }
            0
        }
        WM_COMMAND => {
            if (wparam & 0xffff) == BUTTON_ID {
                if let Some(state) = state_mut(window) {
                    state.send_composer();
                }
                0
            } else {
                unsafe { DefWindowProcW(window, message, wparam, lparam) }
            }
        }
        WM_CHAR => {
            if let Some(state) = state_mut(window) {
                state.character(wparam as u32);
            }
            0
        }
        WM_KEYDOWN => {
            if let Some(state) = state_mut(window) {
                state.key_down(wparam as u32);
            }
            0
        }
        WM_SETFOCUS => 0,
        WM_ERASEBKGND => 1,
        WM_CLOSE => {
            unsafe { DestroyWindow(window) };
            0
        }
        WM_DESTROY => {
            unsafe { PostQuitMessage(0) };
            0
        }
        WM_NCDESTROY => {
            let pointer = unsafe { GetWindowLongPtrW(window, GWLP_USERDATA) } as *mut AppState;
            if !pointer.is_null() {
                unsafe {
                    SetWindowLongPtrW(window, GWLP_USERDATA, 0);
                    drop(Box::from_raw(pointer));
                }
            }
            unsafe { DefWindowProcW(window, message, wparam, lparam) }
        }
        _ => unsafe { DefWindowProcW(window, message, wparam, lparam) },
    }
}

fn state_mut(window: HWND) -> Option<&'static mut AppState> {
    let pointer = unsafe { GetWindowLongPtrW(window, GWLP_USERDATA) } as *mut AppState;
    unsafe { pointer.as_mut() }
}

enum PtyEvent {
    Output(Vec<u8>),
    Exited(u32),
    Error(String),
}

struct TerminalTab {
    id: u64,
    index: u32,
    title: String,
    command_name: String,
    process_id: Option<u32>,
    composer: String,
    parser: vt100::Parser,
    receiver: Receiver<PtyEvent>,
    master: PtyMaster,
    child: PtyChild,
    exited: Option<u32>,
    error: Option<String>,
    last_size: (u16, u16),
    input_bytes: usize,
    output_bytes: usize,
    raw_output: Vec<u8>,
}

impl TerminalTab {
    fn spawn(
        id: u64,
        index: u32,
        title: Option<String>,
        command_line: Vec<String>,
        window: HWND,
    ) -> Result<Self> {
        let program = command_line.first().cloned().unwrap_or_else(|| {
            env::var("COMSPEC")
                .unwrap_or_else(|_| r"C:\Windows\System32\cmd.exe".to_owned())
        });
        let mut command = ChildCommand::new(&program)
            .size(TerminalSize {
                rows: INITIAL_ROWS,
                cols: INITIAL_COLS,
            })
            .env("TERM", "xterm-256color")
            .env("COLORTERM", "truecolor")
            .env("TERM_PROGRAM", "RustCmd");
        for argument in command_line.iter().skip(1) {
            command = command.arg(argument);
        }
        if let Ok(directory) = env::current_dir() {
            command = command.current_dir(directory);
        }

        let spawned = command
            .spawn()
            .context("failed to start terminal command")?;
        let (mut master, child) = spawned.into_parts();
        let process_id = Some(child.pid().as_u32());
        let reader = master
            .try_clone_for_startup_reader()
            .context("failed to clone ConPTY reader")?;
        let mut wait_child = child
            .try_clone_for_wait()
            .context("failed to clone ConPTY child wait handle")?;
        let (sender, receiver) = mpsc::channel();
        let wake_window = window as isize;

        let output_sender = sender.clone();
        thread::spawn(move || {
            let mut buffer = [0_u8; 16 * 1024];
            loop {
                match reader.io().read(&mut buffer) {
                    Ok(0) => break,
                    Ok(size) => {
                        if output_sender
                            .send(PtyEvent::Output(buffer[..size].to_vec()))
                            .is_err()
                        {
                            break;
                        }
                        unsafe {
                            PostMessageW(wake_window as HWND, WM_APP_WAKE, 0, 0);
                        }
                    }
                    Err(error) => {
                        let _ = output_sender.send(PtyEvent::Error(error.to_string()));
                        unsafe {
                            PostMessageW(wake_window as HWND, WM_APP_WAKE, 0, 0);
                        }
                        break;
                    }
                }
            }
        });

        thread::spawn(move || {
            let event = match wait_child.wait() {
                Ok(status) => {
                    wait_child.close_pseudoconsole();
                    PtyEvent::Exited(status.code().unwrap_or(1) as u32)
                }
                Err(error) => PtyEvent::Error(format!("process wait failed: {error}")),
            };
            let _ = sender.send(event);
            unsafe {
                PostMessageW(wake_window as HWND, WM_APP_WAKE, 0, 0);
            }
        });

        let command_name = std::path::Path::new(&program)
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or(&program)
            .to_owned();
        Ok(Self {
            id,
            index,
            title: title.unwrap_or_else(|| command_name.clone()),
            command_name,
            process_id,
            composer: String::new(),
            parser: vt100::Parser::new(INITIAL_ROWS, INITIAL_COLS, SCROLLBACK_LINES),
            receiver,
            master,
            child,
            exited: None,
            error: None,
            last_size: (INITIAL_ROWS, INITIAL_COLS),
            input_bytes: 0,
            output_bytes: 0,
            raw_output: Vec::new(),
        })
    }

    fn poll(&mut self) -> bool {
        let mut changed = false;
        while let Ok(event) = self.receiver.try_recv() {
            changed = true;
            match event {
                PtyEvent::Output(bytes) => {
                    self.output_bytes += bytes.len();
                    self.parser.process(&bytes);
                    self.raw_output.extend_from_slice(&bytes);
                    if self.raw_output.len() > RAW_OUTPUT_LIMIT {
                        let excess = self.raw_output.len() - RAW_OUTPUT_LIMIT;
                        self.raw_output.drain(..excess);
                    }
                }
                PtyEvent::Exited(code) => self.exited = Some(code),
                PtyEvent::Error(error) => self.error = Some(error),
            }
        }
        changed
    }

    fn resize(&mut self, rows: u16, cols: u16) {
        if self.last_size == (rows, cols) {
            return;
        }
        self.last_size = (rows, cols);
        self.parser.screen_mut().set_size(rows, cols);
        if let Err(error) = self.master.resize(TerminalSize {
            rows,
            cols,
        }) {
            self.error = Some(format!("resize failed: {error}"));
        }
    }

    fn send(&mut self, bytes: &[u8]) {
        if self.exited.is_some() {
            return;
        }
        if let Err(error) = self.master.write_all(bytes) {
            self.error = Some(format!("input failed: {error}"));
        } else {
            self.input_bytes += bytes.len();
        }
    }

    fn send_native_mouse_click(&mut self, x: u16, y: u16) -> Result<()> {
        if self.exited.is_some() {
            anyhow::bail!("process has exited");
        }
        let raw_pid = self.process_id.context("process id is unavailable")?;
        let process_id = ProcessId::new(raw_pid).context("invalid process id")?;
        let x = i16::try_from(x).context("mouse x coordinate is too large")?;
        let y = i16::try_from(y).context("mouse y coordinate is too large")?;
        write_windows_console_mouse_drag(process_id, x, y, x, y)
            .context("WriteConsoleInputW click failed")?;
        self.input_bytes += 3;
        Ok(())
    }

    fn send_rmux_status_click(&mut self, x: u16, y: u16) -> bool {
        let (rows, cols) = self.last_size;
        if rows < 3 || y < rows - 3 || y >= rows {
            return false;
        }
        let (target, active, mut windows) = {
            let screen = self.parser.screen();
            let mut target = None;
            let mut active = None;
            let mut windows = Vec::new();
            for row in rows - 3..rows {
                let mut line = String::with_capacity(cols as usize);
                for col in 0..cols {
                    let text = screen
                        .cell(row, col)
                        .map(|cell| cell.contents())
                        .unwrap_or(" ");
                    line.push(if text.is_empty() {
                        ' '
                    } else {
                        text.chars().next().unwrap_or(' ')
                    });
                }
                for status_window in parse_status_windows(&line) {
                    if status_window.active {
                        active = Some(status_window.index);
                    }
                    if row == y
                        && usize::from(x) >= status_window.start
                        && usize::from(x) < status_window.end
                    {
                        target = Some(status_window.index);
                    }
                    if !windows.contains(&status_window.index) {
                        windows.push(status_window.index);
                    }
                }
            }
            (target, active, windows)
        };
        let (Some(target), Some(active)) = (target, active) else {
            return false;
        };
        if target == active {
            return true;
        }
        windows.sort_unstable();
        let Some(active_position) = windows.iter().position(|index| *index == active) else {
            return false;
        };
        let Some(target_position) = windows.iter().position(|index| *index == target) else {
            return false;
        };
        let forward = (target_position + windows.len() - active_position) % windows.len();
        let backward = (active_position + windows.len() - target_position) % windows.len();
        let (key, repeats) = if forward <= backward {
            (b"\x1bOS".as_slice(), forward)
        } else {
            (b"\x1bOR".as_slice(), backward)
        };
        for _ in 0..repeats {
            self.send(key);
        }
        true
    }

    fn close_process(&mut self) {
        if self.exited.is_none() {
            let _ = self.child.terminate_forcefully();
        }
    }
}

impl Drop for TerminalTab {
    fn drop(&mut self) {
        self.close_process();
    }
}

struct AppState {
    window: HWND,
    edit: HWND,
    send_button: HWND,
    tabs: Vec<TerminalTab>,
    active: Option<u64>,
    next_id: u64,
    session_name: String,
    started_at: SystemTime,
    ipc_receiver: Receiver<IpcEnvelope>,
    last_error: Option<String>,
    close_requested: bool,
}

impl AppState {
    fn new(window: HWND, edit: HWND, send_button: HWND) -> Result<Self> {
        let ipc_receiver = start_ipc_server(window)?;
        let mut state = Self {
            window,
            edit,
            send_button,
            tabs: Vec::new(),
            active: None,
            next_id: 1,
            session_name: "rustcmd".to_owned(),
            started_at: SystemTime::now(),
            ipc_receiver,
            last_error: None,
            close_requested: false,
        };
        state.create_tab(None, Vec::new(), true)?;
        Ok(state)
    }

    fn create_tab(
        &mut self,
        title: Option<String>,
        command: Vec<String>,
        select: bool,
    ) -> Result<u32> {
        self.save_active_composer();
        let id = self.next_id;
        self.next_id += 1;
        let index = (0..)
            .find(|candidate| !self.tabs.iter().any(|tab| tab.index == *candidate))
            .unwrap_or(self.tabs.len() as u32);
        let tab = TerminalTab::spawn(id, index, title, command, self.window)?;
        self.tabs.push(tab);
        self.tabs.sort_by_key(|tab| tab.index);
        if select {
            self.active = Some(id);
            self.load_active_composer();
        }
        Ok(index)
    }

    fn active_position(&self) -> Option<usize> {
        let active = self.active?;
        self.tabs.iter().position(|tab| tab.id == active)
    }

    fn target_position(&self, target: Option<&str>) -> Option<usize> {
        let Some(target) = target else {
            return self.active_position();
        };
        let target = target
            .rsplit(':')
            .next()
            .unwrap_or(target)
            .trim_start_matches(['=', '%']);
        if let Ok(index) = target.parse::<u32>() {
            return self.tabs.iter().position(|tab| tab.index == index);
        }
        self.tabs.iter().position(|tab| tab.title == target)
    }

    fn close_tab(&mut self, id: u64) {
        self.save_active_composer();
        let Some(position) = self.tabs.iter().position(|tab| tab.id == id) else {
            return;
        };
        self.tabs[position].close_process();
        self.tabs.remove(position);
        if self.active == Some(id) {
            self.active = self
                .tabs
                .get(position)
                .or_else(|| position.checked_sub(1).and_then(|i| self.tabs.get(i)))
                .map(|tab| tab.id);
        }
        self.load_active_composer();
    }

    fn tick(&mut self) -> bool {
        let mut changed = false;
        for tab in &mut self.tabs {
            changed |= tab.poll();
        }
        let envelopes: Vec<IpcEnvelope> = self.ipc_receiver.try_iter().collect();
        changed |= !envelopes.is_empty();
        for envelope in envelopes {
            let response = self.execute_command(&envelope.request.args);
            let _ = envelope.respond_to.send(response);
        }
        if self.close_requested {
            unsafe { PostMessageW(self.window, WM_CLOSE, 0, 0) };
        }
        changed
    }

    fn layout(&self) {
        let mut client: RECT = unsafe { mem::zeroed() };
        unsafe { GetClientRect(self.window, &mut client) };
        let content_width = (client.right - SIDEBAR_WIDTH).max(180);
        let edit_width = (content_width - 104).max(80);
        unsafe {
            MoveWindow(
                self.edit,
                SIDEBAR_WIDTH + 10,
                (client.bottom - COMPOSER_HEIGHT + 10).max(0),
                edit_width,
                COMPOSER_HEIGHT - 20,
                1,
            );
            MoveWindow(
                self.send_button,
                SIDEBAR_WIDTH + 20 + edit_width,
                (client.bottom - COMPOSER_HEIGHT + 23).max(0),
                74,
                34,
                1,
            );
        }
    }

    fn click(&mut self, x: i32, y: i32) {
        if x >= SIDEBAR_WIDTH {
            unsafe { SetFocus(self.window) };
            if let Some((column, row)) = self.terminal_cell_at(x, y)
                && let Some(position) = self.active_position()
                && !self.tabs[position].send_rmux_status_click(column, row)
                && let Err(error) = self.tabs[position].send_native_mouse_click(column, row)
            {
                self.last_error = Some(format!("native mouse input failed: {error}"));
            }
            return;
        }
        if y >= 12 && y <= 46 && x >= SIDEBAR_WIDTH - 48 {
            if let Err(error) = self.create_tab(None, Vec::new(), true) {
                self.last_error = Some(format!("{error:#}"));
            }
            unsafe { InvalidateRect(self.window, ptr::null(), 0) };
            return;
        }
        if y < TAB_TOP {
            return;
        }
        let position = ((y - TAB_TOP) / TAB_HEIGHT) as usize;
        if position >= self.tabs.len() {
            return;
        }
        let id = self.tabs[position].id;
        if x >= SIDEBAR_WIDTH - 36 {
            self.close_tab(id);
        } else {
            self.save_active_composer();
            self.active = Some(id);
            self.load_active_composer();
        }
        unsafe {
            SetFocus(self.window);
            InvalidateRect(self.window, ptr::null(), 0);
        }
    }

    fn terminal_cell_at(&self, x: i32, y: i32) -> Option<(u16, u16)> {
        let mut client: RECT = unsafe { mem::zeroed() };
        unsafe { GetClientRect(self.window, &mut client) };
        if x < SIDEBAR_WIDTH
            || y < 0
            || y >= client.bottom.saturating_sub(COMPOSER_HEIGHT)
        {
            return None;
        }

        let device = unsafe { GetDC(self.window) };
        if device.is_null() {
            return None;
        }
        let font = unsafe { GetStockObject(SYSTEM_FIXED_FONT) as HFONT };
        let previous_font = unsafe { SelectObject(device, font as HGDIOBJ) };
        let mut metrics: TEXTMETRICW = unsafe { mem::zeroed() };
        let mut extent: SIZE = unsafe { mem::zeroed() };
        let sample = ['W' as u16];
        unsafe {
            GetTextMetricsW(device, &mut metrics);
            GetTextExtentPoint32W(device, sample.as_ptr(), 1, &mut extent);
            SelectObject(device, previous_font);
            ReleaseDC(self.window, device);
        }
        let cell_width = extent.cx.max(7);
        let cell_height = (metrics.tmHeight + metrics.tmExternalLeading).max(14);
        let column = ((x - SIDEBAR_WIDTH) / cell_width).clamp(0, u16::MAX as i32) as u16;
        let row = (y / cell_height).clamp(0, u16::MAX as i32) as u16;
        Some((column, row))
    }

    fn character(&mut self, codepoint: u32) {
        let Some(position) = self.active_position() else {
            return;
        };
        match codepoint {
            8 => self.tabs[position].send(b"\x08"),
            9 => self.tabs[position].send(b"\t"),
            13 => self.tabs[position].send(b"\r"),
            27 => self.tabs[position].send(b"\x1b"),
            value => {
                if let Some(character) = char::from_u32(value) {
                    let mut buffer = [0_u8; 4];
                    self.tabs[position].send(character.encode_utf8(&mut buffer).as_bytes());
                }
            }
        }
    }

    fn key_down(&mut self, virtual_key: u32) {
        let Some(position) = self.active_position() else {
            return;
        };
        let bytes = match virtual_key {
            0x21 => Some(b"\x1b[5~".as_slice()),
            0x22 => Some(b"\x1b[6~".as_slice()),
            0x23 => Some(b"\x1b[F".as_slice()),
            0x24 => Some(b"\x1b[H".as_slice()),
            0x25 => Some(b"\x1b[D".as_slice()),
            0x26 => Some(b"\x1b[A".as_slice()),
            0x27 => Some(b"\x1b[C".as_slice()),
            0x28 => Some(b"\x1b[B".as_slice()),
            0x2e => Some(b"\x1b[3~".as_slice()),
            0x70 => Some(b"\x1bOP".as_slice()),
            0x71 => Some(b"\x1bOQ".as_slice()),
            0x72 => Some(b"\x1bOR".as_slice()),
            0x73 => Some(b"\x1bOS".as_slice()),
            0x74 => Some(b"\x1b[15~".as_slice()),
            0x75 => Some(b"\x1b[17~".as_slice()),
            0x76 => Some(b"\x1b[18~".as_slice()),
            0x77 => Some(b"\x1b[19~".as_slice()),
            0x78 => Some(b"\x1b[20~".as_slice()),
            0x79 => Some(b"\x1b[21~".as_slice()),
            0x7a => Some(b"\x1b[23~".as_slice()),
            0x7b => Some(b"\x1b[24~".as_slice()),
            _ => None,
        };
        if let Some(bytes) = bytes {
            self.tabs[position].send(bytes);
        }
    }

    fn save_active_composer(&mut self) {
        let Some(position) = self.active_position() else {
            return;
        };
        self.tabs[position].composer = window_text(self.edit);
    }

    fn load_active_composer(&self) {
        let text = self
            .active_position()
            .and_then(|position| self.tabs.get(position))
            .map(|tab| tab.composer.as_str())
            .unwrap_or("");
        let text = wide(text);
        unsafe { SetWindowTextW(self.edit, text.as_ptr()) };
    }

    fn send_composer(&mut self) {
        let Some(position) = self.active_position() else {
            return;
        };
        let text = window_text(self.edit);
        self.tabs[position].composer = text.clone();
        if text.is_empty() || self.tabs[position].exited.is_some() {
            return;
        }
        self.tabs[position].send(text.as_bytes());
        self.tabs[position].send(b"\r");
        self.tabs[position].composer.clear();
        unsafe { SetWindowTextW(self.edit, wide("").as_ptr()) };
    }

    fn paint(&mut self) {
        let mut paint: PAINTSTRUCT = unsafe { mem::zeroed() };
        let paint_device = unsafe { BeginPaint(self.window, &mut paint) };
        if paint_device.is_null() {
            return;
        }
        let mut client: RECT = unsafe { mem::zeroed() };
        unsafe { GetClientRect(self.window, &mut client) };
        let width = client.right.max(1);
        let height = client.bottom.max(1);
        let buffer_device = unsafe { CreateCompatibleDC(paint_device) };
        let buffer_bitmap = unsafe { CreateCompatibleBitmap(paint_device, width, height) };
        let buffered = !buffer_device.is_null() && !buffer_bitmap.is_null();
        let previous_bitmap = if buffered {
            unsafe { SelectObject(buffer_device, buffer_bitmap as HGDIOBJ) }
        } else {
            ptr::null_mut()
        };
        let device = if buffered {
            buffer_device
        } else {
            paint_device
        };

        fill(device, &client, COLOR_TERMINAL);
        fill(
            device,
            &RECT {
                left: 0,
                top: 0,
                right: SIDEBAR_WIDTH,
                bottom: client.bottom,
            },
            COLOR_SIDEBAR,
        );
        fill(
            device,
            &RECT {
                left: SIDEBAR_WIDTH,
                top: (client.bottom - COMPOSER_HEIGHT).max(0),
                right: client.right,
                bottom: client.bottom,
            },
            COLOR_COMPOSER,
        );

        let ui_font = unsafe { GetStockObject(DEFAULT_GUI_FONT) as HFONT };
        let terminal_font = unsafe { GetStockObject(SYSTEM_FIXED_FONT) as HFONT };
        unsafe {
            SelectObject(device, ui_font as HGDIOBJ);
            SetBkMode(device, TRANSPARENT as i32);
        }
        draw_text(
            device,
            "RustCmd",
            RECT {
                left: 14,
                top: 10,
                right: 145,
                bottom: 48,
            },
            COLOR_TEXT,
            DT_LEFT | DT_SINGLELINE | DT_VCENTER,
        );
        draw_text(
            device,
            "+",
            RECT {
                left: SIDEBAR_WIDTH - 46,
                top: 10,
                right: SIDEBAR_WIDTH - 10,
                bottom: 47,
            },
            COLOR_GREEN,
            DT_LEFT | DT_SINGLELINE | DT_VCENTER,
        );

        for (position, tab) in self.tabs.iter().enumerate() {
            let top = TAB_TOP + position as i32 * TAB_HEIGHT;
            let rect = RECT {
                left: 6,
                top,
                right: SIDEBAR_WIDTH - 6,
                bottom: top + TAB_HEIGHT - 3,
            };
            if self.active == Some(tab.id) {
                fill(device, &rect, COLOR_ACTIVE);
            }
            fill(
                device,
                &RECT {
                    left: 12,
                    top: top + 13,
                    right: 20,
                    bottom: top + 21,
                },
                if tab.exited.is_some() {
                    COLOR_ORANGE
                } else {
                    COLOR_GREEN
                },
            );
            let label = match tab.exited {
                Some(code) => format!("{}:{} [exit {code}]", tab.index, tab.title),
                None => format!("{}:{}", tab.index, tab.title),
            };
            draw_text(
                device,
                &label,
                RECT {
                    left: 27,
                    top,
                    right: SIDEBAR_WIDTH - 39,
                    bottom: top + TAB_HEIGHT - 3,
                },
                COLOR_TEXT,
                DT_LEFT | DT_SINGLELINE | DT_VCENTER | DT_END_ELLIPSIS,
            );
            draw_text(
                device,
                "x",
                RECT {
                    left: SIDEBAR_WIDTH - 31,
                    top,
                    right: SIDEBAR_WIDTH - 9,
                    bottom: top + TAB_HEIGHT - 3,
                },
                COLOR_MUTED,
                DT_LEFT | DT_SINGLELINE | DT_VCENTER,
            );
        }

        if let Some(position) = self.active_position() {
            unsafe { SelectObject(device, terminal_font as HGDIOBJ) };
            self.paint_terminal(device, position, &client);
        } else {
            draw_text(
                device,
                "Click + to create a cmd.exe tab",
                RECT {
                    left: SIDEBAR_WIDTH + 24,
                    top: 24,
                    right: client.right - 24,
                    bottom: 64,
                },
                COLOR_MUTED,
                DT_LEFT | DT_SINGLELINE | DT_VCENTER,
            );
        }

        if let Some(error) = &self.last_error {
            draw_text(
                device,
                error,
                RECT {
                    left: SIDEBAR_WIDTH + 10,
                    top: (client.bottom - COMPOSER_HEIGHT - 28).max(0),
                    right: client.right - 10,
                    bottom: (client.bottom - COMPOSER_HEIGHT).max(0),
                },
                COLOR_RED,
                DT_LEFT | DT_SINGLELINE | DT_END_ELLIPSIS,
            );
        }
        unsafe {
            if buffered {
                BitBlt(
                    paint_device,
                    0,
                    0,
                    width,
                    height,
                    buffer_device,
                    0,
                    0,
                    SRCCOPY,
                );
                SelectObject(buffer_device, previous_bitmap);
            }
            if !buffer_bitmap.is_null() {
                DeleteObject(buffer_bitmap as HGDIOBJ);
            }
            if !buffer_device.is_null() {
                DeleteDC(buffer_device);
            }
            EndPaint(self.window, &paint);
        };
    }

    fn paint_terminal(&mut self, device: HDC, position: usize, client: &RECT) {
        let mut metrics: TEXTMETRICW = unsafe { mem::zeroed() };
        unsafe { GetTextMetricsW(device, &mut metrics) };
        let mut extent: SIZE = unsafe { mem::zeroed() };
        let sample = ['W' as u16];
        unsafe { GetTextExtentPoint32W(device, sample.as_ptr(), 1, &mut extent) };
        let cell_width = extent.cx.max(7);
        let cell_height = (metrics.tmHeight + metrics.tmExternalLeading).max(14);
        let width = (client.right - SIDEBAR_WIDTH).max(cell_width * 10);
        let height = (client.bottom - COMPOSER_HEIGHT).max(cell_height * 2);
        let cols = (width / cell_width).clamp(10, u16::MAX as i32) as u16;
        let rows = (height / cell_height).clamp(2, u16::MAX as i32) as u16;
        self.tabs[position].resize(rows, cols);

        let screen = self.tabs[position].parser.screen();
        for row in 0..rows {
            let mut runs: Vec<(u16, String, COLORREF, COLORREF)> = Vec::new();
            for col in 0..cols {
                let cell = screen.cell(row, col);
                let (text, mut foreground, mut background) = match cell {
                    Some(cell) => {
                        let text = if cell.is_wide_continuation() || cell.contents().is_empty() {
                            " "
                        } else {
                            cell.contents()
                        };
                        (
                            text,
                            terminal_color(cell.fgcolor(), false),
                            terminal_color(cell.bgcolor(), true),
                        )
                    }
                    None => (" ", COLOR_TEXT, COLOR_TERMINAL),
                };
                if cell.is_some_and(|value| value.inverse()) {
                    mem::swap(&mut foreground, &mut background);
                }
                if let Some((_, run_text, run_foreground, run_background)) = runs.last_mut()
                    && *run_foreground == foreground
                    && *run_background == background
                {
                    run_text.push_str(text);
                } else {
                    runs.push((col, text.to_owned(), foreground, background));
                }
            }
            for (start_col, text, foreground, background) in runs {
                let encoded: Vec<u16> = text.encode_utf16().collect();
                let left = SIDEBAR_WIDTH + start_col as i32 * cell_width;
                let top = row as i32 * cell_height;
                let rect = RECT {
                    left,
                    top,
                    right: left + text.chars().count() as i32 * cell_width,
                    bottom: top + cell_height,
                };
                unsafe {
                    SetTextColor(device, foreground);
                    SetBkColor(device, background);
                    ExtTextOutW(
                        device,
                        left,
                        top,
                        ETO_OPAQUE,
                        &rect,
                        encoded.as_ptr(),
                        encoded.len() as u32,
                        ptr::null(),
                    );
                }
            }
        }

        if self.tabs[position].exited.is_none() && !screen.hide_cursor() {
            let (row, col) = screen.cursor_position();
            let cursor = RECT {
                left: SIDEBAR_WIDTH + col as i32 * cell_width,
                top: row as i32 * cell_height,
                right: SIDEBAR_WIDTH + (col as i32 + 1) * cell_width,
                bottom: (row as i32 + 1) * cell_height,
            };
            let brush = unsafe { CreateSolidBrush(rgb(235, 235, 235)) };
            unsafe {
                FrameRect(device, &cursor, brush);
                DeleteObject(brush as HGDIOBJ);
            }
        }

        if let Some(code) = self.tabs[position].exited {
            draw_text(
                device,
                &format!("Process exited with code {code}. This tab remains until you close it."),
                RECT {
                    left: SIDEBAR_WIDTH + 8,
                    top: height - cell_height,
                    right: client.right - 8,
                    bottom: height,
                },
                COLOR_ORANGE,
                DT_LEFT | DT_SINGLELINE | DT_END_ELLIPSIS,
            );
        }
    }

    fn execute_command(&mut self, args: &[String]) -> IpcResponse {
        let Some(command) = args.first().map(String::as_str) else {
            return IpcResponse::failure("no command specified");
        };
        match command {
            "__focus" | "attach" | "attach-session" | "start-server" => {
                unsafe {
                    ShowWindow(self.window, SW_RESTORE);
                    SetForegroundWindow(self.window);
                }
                IpcResponse::success("")
            }
            "new" | "new-session" => {
                if let Some(name) = option_value(args, "-s") {
                    self.session_name = name.to_owned();
                }
                if self.tabs.is_empty() {
                    let (_, _, child_command) = parse_new_command(args);
                    if let Err(error) = self.create_tab(None, child_command, true) {
                        return IpcResponse::failure(format!("{error:#}"));
                    }
                }
                unsafe {
                    ShowWindow(self.window, SW_RESTORE);
                    SetForegroundWindow(self.window);
                }
                IpcResponse::success("")
            }
            "neww" | "new-window" => {
                let (title, detached, child_command) = parse_new_command(args);
                match self.create_tab(title, child_command, !detached) {
                    Ok(index) => IpcResponse::success(index.to_string()),
                    Err(error) => IpcResponse::failure(format!("{error:#}")),
                }
            }
            "ls" | "list-sessions" => IpcResponse::success(format!(
                "{}: {} windows (created {}) (attached)",
                self.session_name,
                self.tabs.len(),
                self.started_at
                    .duration_since(SystemTime::UNIX_EPOCH)
                    .map(|duration| duration.as_secs())
                    .unwrap_or_default()
            )),
            "has" | "has-session" => {
                let requested = option_value(args, "-t");
                if requested.is_none_or(|name| name == self.session_name) {
                    IpcResponse::success("")
                } else {
                    IpcResponse::failure(format!(
                        "can't find session: {}",
                        requested.unwrap_or_default()
                    ))
                }
            }
            "lsw" | "list-windows" => {
                let format = option_value(args, "-F").unwrap_or("#I:#W#{?window_active,*,}");
                IpcResponse::success(
                    self.tabs
                        .iter()
                        .map(|tab| {
                            render_format(
                                format,
                                tab,
                                &self.session_name,
                                self.active == Some(tab.id),
                            )
                        })
                        .collect::<Vec<_>>()
                        .join("\n"),
                )
            }
            "lsp" | "list-panes" => {
                let format = option_value(args, "-F").unwrap_or(
                    "#{pane_id}: [#{pane_width}x#{pane_height}] #{pane_current_command}",
                );
                let all = args.iter().any(|arg| arg == "-a");
                let tabs: Vec<&TerminalTab> = if all {
                    self.tabs.iter().collect()
                } else {
                    self.target_position(option_value(args, "-t"))
                        .and_then(|position| self.tabs.get(position))
                        .into_iter()
                        .collect()
                };
                IpcResponse::success(
                    tabs.into_iter()
                        .map(|tab| {
                            render_format(
                                format,
                                tab,
                                &self.session_name,
                                self.active == Some(tab.id),
                            )
                        })
                        .collect::<Vec<_>>()
                        .join("\n"),
                )
            }
            "selectw" | "select-window" => {
                let target = if args.iter().any(|arg| arg == "-n") {
                    self.adjacent_position(1)
                } else if args.iter().any(|arg| arg == "-p") {
                    self.adjacent_position(-1)
                } else {
                    self.target_position(option_value(args, "-t"))
                };
                let Some(position) = target else {
                    return IpcResponse::failure("can't find window");
                };
                self.save_active_composer();
                self.active = Some(self.tabs[position].id);
                self.load_active_composer();
                unsafe {
                    ShowWindow(self.window, SW_RESTORE);
                    SetForegroundWindow(self.window);
                    InvalidateRect(self.window, ptr::null(), 0);
                }
                IpcResponse::success("")
            }
            "next" | "next-window" => self.select_adjacent(1),
            "prev" | "previous-window" => self.select_adjacent(-1),
            "renamew" | "rename-window" => {
                let Some(name) = last_positional(args, &["-t"]) else {
                    return IpcResponse::failure("usage: rename-window [-t target] new-name");
                };
                let Some(position) = self.target_position(option_value(args, "-t")) else {
                    return IpcResponse::failure("can't find window");
                };
                self.tabs[position].title = name.to_owned();
                IpcResponse::success("")
            }
            "rename" | "rename-session" => {
                let Some(name) = last_positional(args, &["-t"]) else {
                    return IpcResponse::failure("usage: rename-session new-name");
                };
                self.session_name = name.to_owned();
                IpcResponse::success("")
            }
            "killw" | "kill-window" => {
                let Some(position) = self.target_position(option_value(args, "-t")) else {
                    return IpcResponse::failure("can't find window");
                };
                let id = self.tabs[position].id;
                self.close_tab(id);
                IpcResponse::success("")
            }
            "send" | "send-keys" => {
                let Some(position) = self.target_position(option_value(args, "-t")) else {
                    return IpcResponse::failure("can't find pane");
                };
                let literal = args.iter().any(|arg| arg == "-l");
                for key in positional_values(args, &["-t"], &["-l", "-R", "-X"]) {
                    if literal {
                        self.tabs[position].send(key.as_bytes());
                    } else if let Some(bytes) = tmux_key_bytes(key) {
                        self.tabs[position].send(&bytes);
                    } else {
                        self.tabs[position].send(key.as_bytes());
                    }
                }
                IpcResponse::success("")
            }
            "capturep" | "capture-pane" => {
                let Some(position) = self.target_position(option_value(args, "-t")) else {
                    return IpcResponse::failure("can't find pane");
                };
                if args.iter().any(|argument| argument == "--raw-escaped") {
                    return IpcResponse::success(
                        String::from_utf8_lossy(&self.tabs[position].raw_output)
                            .escape_debug()
                            .to_string(),
                    );
                }
                IpcResponse::success(self.tabs[position].parser.screen().contents())
            }
            "dump-cells" => {
                let Some(position) = self.target_position(option_value(args, "-t")) else {
                    return IpcResponse::failure("can't find pane");
                };
                let requested_row = option_value(args, "-r")
                    .and_then(|value| value.parse::<u16>().ok());
                let tab = &self.tabs[position];
                let screen = tab.parser.screen();
                let mut cells = Vec::new();
                for row in 0..tab.last_size.0 {
                    if requested_row.is_some_and(|requested| requested != row) {
                        continue;
                    }
                    for col in 0..tab.last_size.1 {
                        let Some(cell) = screen.cell(row, col) else {
                            continue;
                        };
                        let foreground = format!("{:?}", cell.fgcolor());
                        let background = format!("{:?}", cell.bgcolor());
                        if cell.contents().is_empty()
                            && foreground == "Default"
                            && background == "Default"
                            && !cell.inverse()
                        {
                            continue;
                        }
                        cells.push(serde_json::json!({
                            "row": row,
                            "col": col,
                            "text": cell.contents(),
                            "fg": foreground,
                            "bg": background,
                            "inverse": cell.inverse(),
                            "wide_continuation": cell.is_wide_continuation(),
                        }));
                    }
                }
                match serde_json::to_string_pretty(&serde_json::json!({
                    "window_id": format!("@{}", tab.id),
                    "rows": tab.last_size.0,
                    "cols": tab.last_size.1,
                    "cells": cells,
                })) {
                    Ok(json) => IpcResponse::success(json),
                    Err(error) => IpcResponse::failure(error.to_string()),
                }
            }
            "send-mouse" => {
                let Some(position) = self.target_position(option_value(args, "-t")) else {
                    return IpcResponse::failure("can't find pane");
                };
                let Some(x) = option_value(args, "-x").and_then(|value| value.parse::<u16>().ok())
                else {
                    return IpcResponse::failure("send-mouse requires numeric -x");
                };
                let Some(y) = option_value(args, "-y").and_then(|value| value.parse::<u16>().ok())
                else {
                    return IpcResponse::failure("send-mouse requires numeric -y");
                };
                let button_name = option_value(args, "--button").unwrap_or("left");
                let button = match button_name {
                    "left" => 0,
                    "middle" => 1,
                    "right" => 2,
                    "wheel-up" => 64,
                    "wheel-down" => 65,
                    other => return IpcResponse::failure(format!("unknown mouse button: {other}")),
                };
                let suffix = if option_value(args, "--action") == Some("release") {
                    'm'
                } else {
                    'M'
                };
                let protocol = option_value(args, "--protocol").unwrap_or("auto");
                if protocol == "auto" && self.tabs[position].send_rmux_status_click(x, y) {
                    return IpcResponse::success("");
                }
                if protocol != "sgr" && button_name == "left" {
                    return match self.tabs[position].send_native_mouse_click(x, y) {
                        Ok(()) => IpcResponse::success(""),
                        Err(error) if protocol == "native" => {
                            IpcResponse::failure(format!("{error:#}"))
                        }
                        Err(_) => {
                            self.tabs[position].send(
                                format!("\x1b[<{button};{};{}{suffix}", x + 1, y + 1).as_bytes(),
                            );
                            IpcResponse::success("")
                        }
                    };
                }
                if protocol != "auto" && protocol != "sgr" && protocol != "native" {
                    return IpcResponse::failure(format!("unknown mouse protocol: {protocol}"));
                }
                self.tabs[position].send(
                    format!("\x1b[<{button};{};{}{suffix}", x + 1, y + 1).as_bytes(),
                );
                IpcResponse::success("")
            }
            "active-window" | "active-tab" => {
                let Some(position) = self.active_position() else {
                    return IpcResponse::failure("no active window");
                };
                let format = option_value(args, "-F").unwrap_or("#{window_id}:#{window_name}");
                IpcResponse::success(render_format(
                    format,
                    &self.tabs[position],
                    &self.session_name,
                    true,
                ))
            }
            "show-composer" => {
                self.save_active_composer();
                let Some(position) = self.target_position(option_value(args, "-t")) else {
                    return IpcResponse::failure("can't find window");
                };
                IpcResponse::success(self.tabs[position].composer.clone())
            }
            "set-composer" => {
                let Some(position) = self.target_position(option_value(args, "-t")) else {
                    return IpcResponse::failure("can't find window");
                };
                let text = positional_values(args, &["-t"], &[])
                    .join(" ");
                self.tabs[position].composer = text.clone();
                if self.active == Some(self.tabs[position].id) {
                    unsafe { SetWindowTextW(self.edit, wide(&text).as_ptr()) };
                }
                IpcResponse::success("")
            }
            "send-composer" => {
                self.save_active_composer();
                let Some(position) = self.target_position(option_value(args, "-t")) else {
                    return IpcResponse::failure("can't find window");
                };
                let text = mem::take(&mut self.tabs[position].composer);
                if !text.is_empty() {
                    self.tabs[position].send(text.as_bytes());
                    self.tabs[position].send(b"\r");
                }
                if self.active == Some(self.tabs[position].id) {
                    unsafe { SetWindowTextW(self.edit, wide("").as_ptr()) };
                }
                IpcResponse::success("")
            }
            "inspect" | "pane-snapshot" => {
                self.save_active_composer();
                let selected: Vec<&TerminalTab> = match option_value(args, "-t") {
                    Some(target) => {
                        let Some(position) = self.target_position(Some(target)) else {
                            return IpcResponse::failure("can't find window");
                        };
                        vec![&self.tabs[position]]
                    }
                    None => self.tabs.iter().collect(),
                };
                let windows = selected
                    .into_iter()
                    .map(|tab| {
                        serde_json::json!({
                            "id": format!("@{}", tab.id),
                            "index": tab.index,
                            "name": tab.title,
                            "active": self.active == Some(tab.id),
                            "dead": tab.exited.is_some(),
                            "exit_code": tab.exited,
                            "pid": tab.process_id,
                            "command": tab.command_name,
                            "rows": tab.last_size.0,
                            "cols": tab.last_size.1,
                            "input_bytes": tab.input_bytes,
                            "output_bytes": tab.output_bytes,
                            "composer": tab.composer,
                            "error": tab.error,
                            "text": tab.parser.screen().contents(),
                        })
                    })
                    .collect::<Vec<_>>();
                match serde_json::to_string_pretty(&serde_json::json!({
                    "session": self.session_name,
                    "active_window_id": self.active.map(|id| format!("@{id}")),
                    "windows": windows,
                })) {
                    Ok(json) => IpcResponse::success(json),
                    Err(error) => IpcResponse::failure(error.to_string()),
                }
            }
            "screenshot" => {
                unsafe {
                    InvalidateRect(self.window, ptr::null(), 0);
                }
                self.paint();
                let path = screenshot_output_path(args, "rustcmd-window");
                match save_window_png(self.window, &path, false) {
                    Ok(()) => IpcResponse::success(path.display().to_string()),
                    Err(error) => IpcResponse::failure(format!("{error:#}")),
                }
            }
            "screenshot-pane" | "screenshot-tab" => {
                let Some(position) = self.target_position(option_value(args, "-t")) else {
                    return IpcResponse::failure("can't find window");
                };
                self.save_active_composer();
                let previous = self.active;
                self.active = Some(self.tabs[position].id);
                self.load_active_composer();
                unsafe {
                    InvalidateRect(self.window, ptr::null(), 0);
                }
                self.paint();
                let path = screenshot_output_path(args, "rustcmd-pane");
                let result = save_window_png(self.window, &path, true);
                self.save_active_composer();
                self.active = previous;
                self.load_active_composer();
                unsafe {
                    InvalidateRect(self.window, ptr::null(), 0);
                }
                match result {
                    Ok(()) => IpcResponse::success(path.display().to_string()),
                    Err(error) => IpcResponse::failure(format!("{error:#}")),
                }
            }
            "display" | "display-message" => {
                let Some(position) = self.target_position(option_value(args, "-t")) else {
                    return IpcResponse::failure("can't find pane");
                };
                let format = last_positional(args, &["-t"])
                    .unwrap_or("#{session_name}:#{window_index}.#{pane_index}");
                IpcResponse::success(render_format(
                    format,
                    &self.tabs[position],
                    &self.session_name,
                    self.active == Some(self.tabs[position].id),
                ))
            }
            "show" | "show-options" => IpcResponse::success(
                [
                    format!("default-shell {}", env::var("COMSPEC").unwrap_or_default()),
                    "remain-on-exit on".to_owned(),
                    "status on".to_owned(),
                ]
                .join("\n"),
            ),
            "lscm" | "list-commands" => IpcResponse::success(SUPPORTED_COMMANDS),
            "splitw" | "split-window" => IpcResponse::failure(
                "split-window is not implemented yet; RustCmd currently maps one ConPTY pane per tab",
            ),
            "kill-session" | "kill-server" => {
                for tab in &mut self.tabs {
                    tab.close_process();
                }
                self.tabs.clear();
                self.active = None;
                self.close_requested = true;
                IpcResponse::success("")
            }
            _ => IpcResponse::failure(format!("unknown command: {command}")),
        }
    }

    fn adjacent_position(&self, direction: i32) -> Option<usize> {
        if self.tabs.is_empty() {
            return None;
        }
        let current = self.active_position().unwrap_or(0) as i32;
        Some((current + direction).rem_euclid(self.tabs.len() as i32) as usize)
    }

    fn select_adjacent(&mut self, direction: i32) -> IpcResponse {
        let Some(position) = self.adjacent_position(direction) else {
            return IpcResponse::failure("no windows");
        };
        self.save_active_composer();
        self.active = Some(self.tabs[position].id);
        self.load_active_composer();
        IpcResponse::success("")
    }
}

#[derive(Clone, Copy, Debug)]
struct StatusWindow {
    index: u32,
    start: usize,
    end: usize,
    active: bool,
}

fn parse_status_windows(line: &str) -> Vec<StatusWindow> {
    let bytes = line.as_bytes();
    let mut windows = Vec::new();
    let mut position = 0;
    while position < bytes.len() {
        let bracketed = bytes[position] == b'[';
        let digit_start = position + usize::from(bracketed);
        if digit_start >= bytes.len() || !bytes[digit_start].is_ascii_digit() {
            position += 1;
            continue;
        }
        let mut colon = digit_start;
        while colon < bytes.len() && bytes[colon].is_ascii_digit() {
            colon += 1;
        }
        if colon >= bytes.len() || bytes[colon] != b':' {
            position = colon.max(position + 1);
            continue;
        }
        let mut end = colon + 1;
        while end < bytes.len() && !bytes[end].is_ascii_whitespace() && bytes[end] != b']' {
            end += 1;
        }
        if end == colon + 1 {
            position = end;
            continue;
        }
        if bracketed && end < bytes.len() && bytes[end] == b']' {
            end += 1;
        }
        if let Ok(index) = line[digit_start..colon].parse::<u32>() {
            windows.push(StatusWindow {
                index,
                start: position,
                end,
                active: bracketed,
            });
        }
        position = end;
    }
    windows
}

fn fill(device: HDC, rect: &RECT, color: COLORREF) {
    let brush = unsafe { CreateSolidBrush(color) };
    unsafe {
        FillRect(device, rect, brush);
        DeleteObject(brush as HGDIOBJ);
    }
}

fn draw_text(device: HDC, text: &str, mut rect: RECT, color: COLORREF, format: u32) {
    let encoded = wide(text);
    unsafe {
        SetTextColor(device, color);
        SetBkMode(device, TRANSPARENT as i32);
        DrawTextW(
            device,
            encoded.as_ptr(),
            text.encode_utf16().count() as i32,
            &mut rect,
            format,
        );
    }
}

fn terminal_color(color: vt100::Color, background: bool) -> COLORREF {
    match color {
        vt100::Color::Default if background => COLOR_TERMINAL,
        vt100::Color::Default => COLOR_TEXT,
        vt100::Color::Rgb(red, green, blue) => rgb(red, green, blue),
        vt100::Color::Idx(index) => ansi_color(index),
    }
}

fn ansi_color(index: u8) -> COLORREF {
    const BASIC: [(u8, u8, u8); 16] = [
        (12, 14, 18),
        (205, 73, 69),
        (91, 184, 104),
        (220, 184, 87),
        (84, 132, 214),
        (176, 101, 193),
        (69, 179, 184),
        (214, 220, 230),
        (100, 108, 123),
        (240, 100, 95),
        (121, 215, 135),
        (245, 210, 112),
        (112, 159, 236),
        (205, 132, 222),
        (97, 211, 216),
        (255, 255, 255),
    ];
    if let Some(&(red, green, blue)) = BASIC.get(index as usize) {
        return rgb(red, green, blue);
    }
    if (16..=231).contains(&index) {
        let value = index - 16;
        let component = |part: u8| if part == 0 { 0 } else { 55 + part * 40 };
        return rgb(
            component(value / 36),
            component((value / 6) % 6),
            component(value % 6),
        );
    }
    let gray = 8 + index.saturating_sub(232) * 10;
    rgb(gray, gray, gray)
}

fn window_text(window: HWND) -> String {
    let length = unsafe { GetWindowTextLengthW(window) }.max(0) as usize;
    let mut buffer = vec![0_u16; length + 1];
    let copied = unsafe { GetWindowTextW(window, buffer.as_mut_ptr(), buffer.len() as i32) };
    String::from_utf16_lossy(&buffer[..copied.max(0) as usize])
}

fn wide(value: &str) -> Vec<u16> {
    value.encode_utf16().chain(Some(0)).collect()
}

fn ipc_address() -> String {
    if let Ok(address) = env::var("RUSTCMD_IPC_ADDRESS")
        && !address.trim().is_empty()
    {
        return address;
    }
    let user = env::var("USERNAME").unwrap_or_else(|_| "default".to_owned());
    let hash = user.bytes().fold(2_166_136_261_u32, |hash, byte| {
        (hash ^ u32::from(byte)).wrapping_mul(16_777_619)
    });
    format!("127.0.0.1:{}", 42_000 + hash % 10_000)
}

fn send_ipc_request(args: Vec<String>) -> Result<IpcResponse> {
    use std::io::BufRead as _;
    let mut connection =
        std::net::TcpStream::connect(ipc_address()).context("RustCmd server is not running")?;
    connection.write_all(serde_json::to_string(&IpcRequest { args })?.as_bytes())?;
    connection.write_all(b"\n")?;
    connection.flush()?;
    let mut reader = std::io::BufReader::new(connection);
    let mut response = String::new();
    reader.read_line(&mut response)?;
    if response.is_empty() {
        anyhow::bail!("RustCmd server closed the IPC connection");
    }
    serde_json::from_str(&response).context("invalid response from RustCmd server")
}

fn start_ipc_server(window: HWND) -> Result<Receiver<IpcEnvelope>> {
    use std::io::BufRead as _;
    let listener = std::net::TcpListener::bind(ipc_address())
        .context("another RustCmd server is already using the local IPC port")?;
    let (sender, receiver) = mpsc::channel();
    let wake_window = window as isize;
    thread::spawn(move || {
        for connection in listener.incoming() {
            let Ok(connection) = connection else {
                continue;
            };
            let mut reader = std::io::BufReader::new(connection);
            let mut line = String::new();
            let response = if reader.read_line(&mut line).is_err() {
                IpcResponse::failure("could not read IPC request")
            } else {
                match serde_json::from_str::<IpcRequest>(&line) {
                    Ok(request) => {
                        let (response_sender, response_receiver) = mpsc::channel();
                        if sender
                            .send(IpcEnvelope {
                                request,
                                respond_to: response_sender,
                            })
                            .is_err()
                        {
                            IpcResponse::failure("RustCmd GUI is shutting down")
                        } else {
                            unsafe {
                                PostMessageW(wake_window as HWND, WM_APP_WAKE, 0, 0);
                            }
                            response_receiver.recv_timeout(IPC_TIMEOUT).unwrap_or_else(|_| {
                                IpcResponse::failure("RustCmd GUI did not process the command")
                            })
                        }
                    }
                    Err(error) => IpcResponse::failure(format!("invalid IPC request: {error}")),
                }
            };
            if let Ok(serialized) = serde_json::to_string(&response) {
                let connection = reader.get_mut();
                let _ = connection.write_all(serialized.as_bytes());
                let _ = connection.write_all(b"\n");
                let _ = connection.flush();
            }
        }
    });
    Ok(receiver)
}

fn run_cli(arguments: Vec<String>) -> i32 {
    let command = arguments.first().map(String::as_str).unwrap_or_default();
    if matches!(command, "wait-pane" | "expect-pane") {
        return run_wait_pane(&arguments);
    }
    let may_start_server = matches!(
        command,
        "new-session"
            | "new"
            | "new-window"
            | "neww"
            | "attach-session"
            | "attach"
            | "start-server"
    );
    let mut response = send_ipc_request(arguments.clone());
    if response.is_err() && may_start_server {
        if let Ok(executable) = env::current_exe() {
            let executable = wide(&executable.to_string_lossy());
            let operation = wide("open");
            let launched = unsafe {
                ShellExecuteW(
                    ptr::null_mut(),
                    operation.as_ptr(),
                    executable.as_ptr(),
                    ptr::null(),
                    ptr::null(),
                    SW_SHOWNORMAL,
                )
            } as isize;
            if launched <= 32 {
                eprintln!("failed to launch RustCmd GUI through Windows Shell ({launched})");
            }
            for _ in 0..40 {
                thread::sleep(Duration::from_millis(100));
                response = send_ipc_request(arguments.clone());
                if response.is_ok() {
                    break;
                }
            }
        }
    }
    match response {
        Ok(response) if response.ok => {
            if !response.output.is_empty() {
                print!("{}", response.output);
                if !response.output.ends_with('\n') {
                    println!();
                }
            }
            0
        }
        Ok(response) => {
            eprintln!("{}", response.error);
            1
        }
        Err(error) => {
            eprintln!("{error:#}");
            1
        }
    }
}

fn run_wait_pane(arguments: &[String]) -> i32 {
    let target = option_value(arguments, "-t").map(str::to_owned);
    let timeout_ms = option_value(arguments, "--timeout-ms")
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(5_000);
    let contains = option_value(arguments, "--contains")
        .map(str::to_owned)
        .or_else(|| {
            (arguments.first().is_some_and(|value| value == "expect-pane"))
                .then(|| last_positional(arguments, &["-t", "--timeout-ms"]))
                .flatten()
                .map(str::to_owned)
        });
    let wait_dead = arguments.iter().any(|argument| argument == "--dead");
    if contains.is_none() && !wait_dead {
        eprintln!("usage: wait-pane [-t target] (--contains text | --dead) [--timeout-ms ms]");
        return 2;
    }
    let deadline = std::time::Instant::now() + Duration::from_millis(timeout_ms);
    loop {
        let matched = if wait_dead {
            let mut request = vec![
                "display-message".to_owned(),
                "-p".to_owned(),
                "#{pane_dead}".to_owned(),
            ];
            if let Some(target) = &target {
                request.extend(["-t".to_owned(), target.clone()]);
            }
            send_ipc_request(request)
                .is_ok_and(|response| response.ok && response.output.trim() == "1")
        } else {
            let mut request = vec!["capture-pane".to_owned(), "-p".to_owned()];
            if let Some(target) = &target {
                request.extend(["-t".to_owned(), target.clone()]);
            }
            send_ipc_request(request).is_ok_and(|response| {
                response.ok
                    && contains
                        .as_ref()
                        .is_some_and(|needle| response.output.contains(needle))
            })
        };
        if matched {
            return 0;
        }
        if std::time::Instant::now() >= deadline {
            eprintln!("wait-pane timed out after {timeout_ms} ms");
            return 1;
        }
        thread::sleep(Duration::from_millis(50));
    }
}

fn hide_owned_console_window() {
    #[link(name = "Kernel32")]
    unsafe extern "system" {
        fn GetConsoleProcessList(process_list: *mut u32, process_count: u32) -> u32;
        fn GetConsoleWindow() -> isize;
    }
    #[link(name = "User32")]
    unsafe extern "system" {
        fn ShowWindow(window: isize, command: i32) -> i32;
    }
    let mut processes = [0_u32; 2];
    let count = unsafe { GetConsoleProcessList(processes.as_mut_ptr(), processes.len() as u32) };
    if count == 1 {
        let window = unsafe { GetConsoleWindow() };
        if window != 0 {
            let _ = unsafe { ShowWindow(window, 0) };
        }
    }
}

const SUPPORTED_COMMANDS: &str = "\
attach-session (attach)
active-window (active-tab)
capture-pane (capturep)
display-message (display)
dump-cells
has-session (has)
inspect
kill-server
kill-session
kill-window (killw)
list-commands (lscm)
list-panes (lsp)
list-sessions (ls)
list-windows (lsw)
new-session (new)
new-window (neww)
next-window (next)
pane-snapshot
previous-window (prev)
rename-session (rename)
rename-window (renamew)
screenshot
screenshot-pane (screenshot-tab)
select-window (selectw)
send-keys (send)
send-composer
send-mouse
set-composer
show-composer
show-options (show)
start-server";

fn print_help() {
    println!(
        "\
RustCmd - native tabbed cmd.exe and tmux/rmux-style multiplexer

Usage:
  rustcmd
  rustcmd new-session [-s name]
  rustcmd new-window [-dn name] [command [args...]]
  rustcmd list-windows [-F format]
  rustcmd select-window -t target
  rustcmd rename-window [-t target] name
  rustcmd kill-window -t target
  rustcmd send-keys [-t target] key...
  rustcmd capture-pane -p [-t target]
  rustcmd capture-pane --raw-escaped [-t target]
  rustcmd dump-cells [-t target] [-r row]
  rustcmd active-window [-F format]
  rustcmd inspect [-t target]
  rustcmd screenshot [-o file.png]
  rustcmd screenshot-pane [-t target] [-o file.png]
  rustcmd show-composer [-t target]
  rustcmd set-composer [-t target] text
  rustcmd send-composer [-t target]
  rustcmd send-mouse [-t target] -x col -y row [--button left] [--action press]
  rustcmd list-panes [-F format]
  rustcmd list-sessions | has-session | kill-server"
    );
}

fn option_value<'a>(args: &'a [String], option: &str) -> Option<&'a str> {
    args.iter()
        .position(|argument| argument == option)
        .and_then(|position| args.get(position + 1))
        .map(String::as_str)
}

fn parse_new_command(args: &[String]) -> (Option<String>, bool, Vec<String>) {
    let mut title = None;
    let mut detached = false;
    let mut position = 1;
    while position < args.len() {
        match args[position].as_str() {
            "-n" => {
                title = args.get(position + 1).cloned();
                position += 2;
            }
            "-d" => {
                detached = true;
                position += 1;
            }
            "-A" | "-P" | "-E" => position += 1,
            "-s" | "-t" | "-c" | "-F" => position += 2,
            "--" => {
                position += 1;
                break;
            }
            option if option.starts_with('-') => position += 1,
            _ => break,
        }
    }
    (title, detached, args[position..].to_vec())
}

fn positional_values<'a>(
    args: &'a [String],
    value_options: &[&str],
    boolean_options: &[&str],
) -> Vec<&'a str> {
    let mut values = Vec::new();
    let mut position = 1;
    while position < args.len() {
        let argument = args[position].as_str();
        if value_options.contains(&argument) {
            position += 2;
        } else if boolean_options.contains(&argument) {
            position += 1;
        } else if argument == "--" {
            values.extend(args[position + 1..].iter().map(String::as_str));
            break;
        } else if argument.starts_with('-') {
            position += 1;
        } else {
            values.push(argument);
            position += 1;
        }
    }
    values
}

fn last_positional<'a>(args: &'a [String], value_options: &[&str]) -> Option<&'a str> {
    positional_values(args, value_options, &["-p", "-v", "-a", "-g"])
        .last()
        .copied()
}

fn render_format(
    format: &str,
    tab: &TerminalTab,
    session_name: &str,
    active: bool,
) -> String {
    let dead = tab.exited.is_some();
    format
        .replace("#{?pane_dead,dead,}", if dead { "dead" } else { "" })
        .replace("#{?window_active,*,}", if active { "*" } else { "" })
        .replace("#{session_name}", session_name)
        .replace("#{window_index}", &tab.index.to_string())
        .replace("#{window_id}", &format!("@{}", tab.id))
        .replace("#{window_name}", &tab.title)
        .replace("#{window_active}", if active { "1" } else { "0" })
        .replace("#{pane_index}", "0")
        .replace("#{pane_id}", &format!("%{}", tab.id))
        .replace("#{pane_dead}", if dead { "1" } else { "0" })
        .replace(
            "#{pane_pid}",
            &tab.process_id.map(|pid| pid.to_string()).unwrap_or_default(),
        )
        .replace("#{pane_current_command}", &tab.command_name)
        .replace("#{pane_start_command}", &tab.command_name)
        .replace("#{pane_input_bytes}", &tab.input_bytes.to_string())
        .replace("#{pane_output_bytes}", &tab.output_bytes.to_string())
        .replace("#{pane_error}", tab.error.as_deref().unwrap_or(""))
        .replace("#{pane_width}", &tab.last_size.1.to_string())
        .replace("#{pane_height}", &tab.last_size.0.to_string())
        .replace("#{history_limit}", &SCROLLBACK_LINES.to_string())
        .replace("#I", &tab.index.to_string())
        .replace("#W", &tab.title)
        .replace("#S", session_name)
        .replace("#P", "0")
}

fn screenshot_output_path(args: &[String], stem: &str) -> std::path::PathBuf {
    if let Some(path) = option_value(args, "-o")
        .or_else(|| last_positional(args, &["-t", "-o"]))
    {
        return std::path::PathBuf::from(path);
    }
    let timestamp = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or_default();
    env::current_dir()
        .unwrap_or_else(|_| std::path::PathBuf::from("."))
        .join(format!("{stem}-{timestamp}.png"))
}

fn save_window_png(window: HWND, path: &std::path::Path, pane_only: bool) -> Result<()> {
    let mut client: RECT = unsafe { mem::zeroed() };
    let mut outer: RECT = unsafe { mem::zeroed() };
    unsafe {
        GetClientRect(window, &mut client);
        GetWindowRect(window, &mut outer);
    }
    let (source, source_x, source_y, width, height) = if pane_only {
        (
            unsafe { GetDC(window) },
            SIDEBAR_WIDTH,
            0,
            (client.right - SIDEBAR_WIDTH).max(1),
            (client.bottom - COMPOSER_HEIGHT).max(1),
        )
    } else {
        (
            unsafe { GetWindowDC(window) },
            0,
            0,
            (outer.right - outer.left).max(1),
            (outer.bottom - outer.top).max(1),
        )
    };
    if source.is_null() {
        anyhow::bail!("failed to acquire window device context");
    }
    let memory_dc = unsafe { CreateCompatibleDC(source) };
    let bitmap = unsafe { CreateCompatibleBitmap(source, width, height) };
    if memory_dc.is_null() || bitmap.is_null() {
        if !memory_dc.is_null() {
            unsafe { DeleteDC(memory_dc) };
        }
        unsafe { ReleaseDC(window, source) };
        anyhow::bail!("failed to allocate screenshot bitmap");
    }

    let previous = unsafe { SelectObject(memory_dc, bitmap as HGDIOBJ) };
    let copied = unsafe {
        BitBlt(
            memory_dc,
            0,
            0,
            width,
            height,
            source,
            source_x,
            source_y,
            SRCCOPY,
        )
    };
    let mut info: BITMAPINFO = unsafe { mem::zeroed() };
    info.bmiHeader = BITMAPINFOHEADER {
        biSize: mem::size_of::<BITMAPINFOHEADER>() as u32,
        biWidth: width,
        biHeight: -height,
        biPlanes: 1,
        biBitCount: 32,
        biCompression: BI_RGB,
        ..unsafe { mem::zeroed() }
    };
    let mut bgra = vec![0_u8; width as usize * height as usize * 4];
    let scanlines = if copied != 0 {
        unsafe {
            GetDIBits(
                memory_dc,
                bitmap,
                0,
                height as u32,
                bgra.as_mut_ptr().cast(),
                &mut info,
                DIB_RGB_COLORS,
            )
        }
    } else {
        0
    };
    unsafe {
        SelectObject(memory_dc, previous);
        DeleteObject(bitmap as HGDIOBJ);
        DeleteDC(memory_dc);
        ReleaseDC(window, source);
    }
    if copied == 0 || scanlines == 0 {
        anyhow::bail!("BitBlt/GetDIBits failed while capturing the window");
    }

    let mut rgba = Vec::with_capacity(bgra.len());
    for pixel in bgra.chunks_exact(4) {
        rgba.extend_from_slice(&[pixel[2], pixel[1], pixel[0], 255]);
    }
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    let file = std::fs::File::create(path)
        .with_context(|| format!("failed to create {}", path.display()))?;
    let mut encoder = png::Encoder::new(file, width as u32, height as u32);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    let mut writer = encoder.write_header().context("failed to start PNG encoder")?;
    writer
        .write_image_data(&rgba)
        .context("failed to write PNG pixels")?;
    Ok(())
}

fn tmux_key_bytes(key: &str) -> Option<Vec<u8>> {
    let bytes = match key {
        "Enter" => b"\r".as_slice(),
        "Escape" | "Esc" => b"\x1b".as_slice(),
        "Space" => b" ".as_slice(),
        "BSpace" | "Backspace" => b"\x08".as_slice(),
        "Tab" => b"\t".as_slice(),
        "Up" => b"\x1b[A".as_slice(),
        "Down" => b"\x1b[B".as_slice(),
        "Right" => b"\x1b[C".as_slice(),
        "Left" => b"\x1b[D".as_slice(),
        "Home" => b"\x1b[H".as_slice(),
        "End" => b"\x1b[F".as_slice(),
        "DC" | "Delete" => b"\x1b[3~".as_slice(),
        "PPage" | "PageUp" => b"\x1b[5~".as_slice(),
        "NPage" | "PageDown" => b"\x1b[6~".as_slice(),
        "F1" => b"\x1bOP".as_slice(),
        "F2" => b"\x1bOQ".as_slice(),
        "F3" => b"\x1bOR".as_slice(),
        "F4" => b"\x1bOS".as_slice(),
        "F5" => b"\x1b[15~".as_slice(),
        "F6" => b"\x1b[17~".as_slice(),
        "F7" => b"\x1b[18~".as_slice(),
        "F8" => b"\x1b[19~".as_slice(),
        "F9" => b"\x1b[20~".as_slice(),
        "F10" => b"\x1b[21~".as_slice(),
        "F11" => b"\x1b[23~".as_slice(),
        "F12" => b"\x1b[24~".as_slice(),
        _ => {
            if let Some(character) = key.strip_prefix("C-").and_then(|value| {
                let mut characters = value.chars();
                let first = characters.next()?;
                characters.next().is_none().then_some(first)
            }) {
                let upper = character.to_ascii_uppercase();
                if upper.is_ascii_alphabetic() {
                    return Some(vec![(upper as u8) - b'@']);
                }
            }
            return None;
        }
    };
    Some(bytes.to_vec())
}
