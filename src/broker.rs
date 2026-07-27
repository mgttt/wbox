//! Windows OCI filesystem broker transport.
//!
//! This module deliberately exposes only HELLO/PING. Filesystem OPEN is added only after
//! component-wise reparse rejection and fd-backed hostfs have product gates.

use crate::error::{Result, WboxError};
use crate::token::{self, OwnedHandle};
use windows_sys::Win32::Foundation::{
    DuplicateHandle, GetLastError, LocalFree, DUPLICATE_SAME_ACCESS, ERROR_PIPE_CONNECTED,
    GENERIC_READ, GENERIC_WRITE, HANDLE, HLOCAL, INVALID_HANDLE_VALUE,
};
use windows_sys::Win32::Security::Authorization::{
    ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
};
use windows_sys::Win32::Security::Cryptography::{
    BCryptGenRandom, BCRYPT_USE_SYSTEM_PREFERRED_RNG,
};
use windows_sys::Win32::Security::{
    GetTokenInformation, TokenAppContainerSid, TokenUser, PSECURITY_DESCRIPTOR,
    SECURITY_ATTRIBUTES, TOKEN_APPCONTAINER_INFORMATION, TOKEN_QUERY, TOKEN_USER,
};
use windows_sys::Win32::Storage::FileSystem::{
    CreateFileW, FlushFileBuffers, ReadFile, WriteFile, FILE_FLAG_FIRST_PIPE_INSTANCE,
    FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING, PIPE_ACCESS_DUPLEX,
};
use windows_sys::Win32::System::JobObjects::IsProcessInJob;
use windows_sys::Win32::System::Pipes::{
    ConnectNamedPipe, CreateNamedPipeW, DisconnectNamedPipe, PIPE_READMODE_MESSAGE,
    PIPE_REJECT_REMOTE_CLIENTS, PIPE_TYPE_MESSAGE, PIPE_WAIT,
};
use windows_sys::Win32::System::Threading::{GetCurrentProcess, GetProcessId, OpenProcessToken};

const MAGIC: u32 = 0x5742_4f58; // "WBOX"
const VERSION: u16 = 1;
const REQUEST_HEADER_LEN: usize = 24;
const RESPONSE_HEADER_LEN: usize = 24;
const MAX_PAYLOAD: usize = 4096;
const PIPE_BUFFER: u32 = 8192;

const OP_HELLO: u16 = 1;
const OP_PING: u16 = 2;
const OP_OPEN: u16 = 3;
const OPEN_FIXED_LEN: usize = 12;
const MAX_RELATIVE_PATH: usize = 1024;

const STATUS_OK: i32 = 0;
const STATUS_PROTOCOL: i32 = -71; // Linux EPROTO
const STATUS_AUTH: i32 = -13; // Linux EACCES

#[derive(Debug, Clone, PartialEq, Eq)]
struct Request {
    opcode: u16,
    request_id: u64,
    flags: u32,
    payload: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Response {
    opcode: u16,
    request_id: u64,
    status: i32,
    payload: Vec<u8>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DecodeError {
    HeaderSize,
    Magic,
    Version,
    PayloadTooLarge,
    PayloadSize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct OpenRequest {
    mount_id: u32,
    linux_flags: u32,
    mode: u32,
    components: Vec<String>,
}

impl OpenRequest {
    fn decode(request: &Request) -> std::result::Result<Self, &'static str> {
        if request.opcode != OP_OPEN || request.flags != 0 {
            return Err("opcode/flags");
        }
        if request.payload.len() <= OPEN_FIXED_LEN {
            return Err("missing path");
        }
        let mount_id = u32::from_le_bytes(request.payload[0..4].try_into().unwrap());
        let linux_flags = u32::from_le_bytes(request.payload[4..8].try_into().unwrap());
        let mode = u32::from_le_bytes(request.payload[8..12].try_into().unwrap());
        if mount_id == 0 {
            return Err("mount id");
        }
        // First data-plane slice is existing-file read-only. Unsupported write/create flags
        // are rejected before any host object is opened.
        if linux_flags != 0 || mode != 0 {
            return Err("open flags");
        }
        let path_bytes = &request.payload[OPEN_FIXED_LEN..];
        if path_bytes.len() > MAX_RELATIVE_PATH {
            return Err("path too long");
        }
        let path = std::str::from_utf8(path_bytes).map_err(|_| "path utf8")?;
        if path.starts_with('/') || path.contains('\\') || path.contains(':') || path.contains('\0')
        {
            return Err("path syntax");
        }
        let mut components = Vec::new();
        for component in path.split('/') {
            if component.is_empty() || component == "." || component == ".." {
                return Err("path component");
            }
            components.push(component.to_string());
        }
        if components.is_empty() {
            return Err("missing path");
        }
        Ok(Self {
            mount_id,
            linux_flags,
            mode,
            components,
        })
    }
}

impl Request {
    fn encode_header(&self) -> [u8; REQUEST_HEADER_LEN] {
        let mut out = [0u8; REQUEST_HEADER_LEN];
        out[0..4].copy_from_slice(&MAGIC.to_le_bytes());
        out[4..6].copy_from_slice(&VERSION.to_le_bytes());
        out[6..8].copy_from_slice(&self.opcode.to_le_bytes());
        out[8..16].copy_from_slice(&self.request_id.to_le_bytes());
        out[16..20].copy_from_slice(&(self.payload.len() as u32).to_le_bytes());
        out[20..24].copy_from_slice(&self.flags.to_le_bytes());
        out
    }

    fn decode(header: &[u8], payload: Vec<u8>) -> std::result::Result<Self, DecodeError> {
        if header.len() != REQUEST_HEADER_LEN {
            return Err(DecodeError::HeaderSize);
        }
        if u32::from_le_bytes(header[0..4].try_into().unwrap()) != MAGIC {
            return Err(DecodeError::Magic);
        }
        if u16::from_le_bytes(header[4..6].try_into().unwrap()) != VERSION {
            return Err(DecodeError::Version);
        }
        let payload_len = u32::from_le_bytes(header[16..20].try_into().unwrap()) as usize;
        if payload_len > MAX_PAYLOAD {
            return Err(DecodeError::PayloadTooLarge);
        }
        if payload.len() != payload_len {
            return Err(DecodeError::PayloadSize);
        }
        Ok(Self {
            opcode: u16::from_le_bytes(header[6..8].try_into().unwrap()),
            request_id: u64::from_le_bytes(header[8..16].try_into().unwrap()),
            flags: u32::from_le_bytes(header[20..24].try_into().unwrap()),
            payload,
        })
    }
}

impl Response {
    fn encode_header(&self) -> [u8; RESPONSE_HEADER_LEN] {
        let mut out = [0u8; RESPONSE_HEADER_LEN];
        out[0..4].copy_from_slice(&MAGIC.to_le_bytes());
        out[4..6].copy_from_slice(&VERSION.to_le_bytes());
        out[6..8].copy_from_slice(&self.opcode.to_le_bytes());
        out[8..16].copy_from_slice(&self.request_id.to_le_bytes());
        out[16..20].copy_from_slice(&self.status.to_le_bytes());
        out[20..24].copy_from_slice(&(self.payload.len() as u32).to_le_bytes());
        out
    }

    fn decode(header: &[u8], payload: Vec<u8>) -> std::result::Result<Self, DecodeError> {
        if header.len() != RESPONSE_HEADER_LEN {
            return Err(DecodeError::HeaderSize);
        }
        if u32::from_le_bytes(header[0..4].try_into().unwrap()) != MAGIC {
            return Err(DecodeError::Magic);
        }
        if u16::from_le_bytes(header[4..6].try_into().unwrap()) != VERSION {
            return Err(DecodeError::Version);
        }
        let payload_len = u32::from_le_bytes(header[20..24].try_into().unwrap()) as usize;
        if payload_len > MAX_PAYLOAD {
            return Err(DecodeError::PayloadTooLarge);
        }
        if payload.len() != payload_len {
            return Err(DecodeError::PayloadSize);
        }
        Ok(Self {
            opcode: u16::from_le_bytes(header[6..8].try_into().unwrap()),
            request_id: u64::from_le_bytes(header[8..16].try_into().unwrap()),
            status: i32::from_le_bytes(header[16..20].try_into().unwrap()),
            payload,
        })
    }
}

pub(crate) struct BrokerEndpoint {
    pipe: OwnedHandle,
    client: OwnedHandle,
    appcontainer_sid: String,
    generation: u64,
    nonce: [u8; 16],
}

impl BrokerEndpoint {
    pub(crate) fn create(appcontainer_sid: &str) -> Result<Self> {
        let mut random = [0u8; 32];
        let status = unsafe {
            BCryptGenRandom(
                std::ptr::null_mut(),
                random.as_mut_ptr(),
                random.len() as u32,
                BCRYPT_USE_SYSTEM_PREFERRED_RNG,
            )
        };
        if status < 0 {
            return Err(WboxError::spawn(format!(
                "BCryptGenRandom(broker endpoint) 失败，NTSTATUS=0x{:08X}",
                status as u32
            )));
        }
        let generation = u64::from_le_bytes(random[0..8].try_into().unwrap());
        let nonce: [u8; 16] = random[8..24].try_into().unwrap();
        let suffix = hex(&random[24..32]);
        let name = format!(
            r"\\.\pipe\LOCAL\wbox.{}.{}.{}",
            std::process::id(),
            generation,
            suffix
        );

        let user_sid = current_user_sid_string()?;
        // Guest never opens this name. The supervisor connects both ends before spawn and
        // inherits only the connected client HANDLE, so the named object remains owner-only.
        let sddl = format!("D:P(A;;GA;;;{})", user_sid);
        let sddl_wide = token::to_wide(&sddl);
        let mut descriptor: PSECURITY_DESCRIPTOR = std::ptr::null_mut();
        let ok = unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                sddl_wide.as_ptr(),
                SDDL_REVISION_1,
                &mut descriptor,
                std::ptr::null_mut(),
            )
        };
        if ok == 0 {
            return Err(last_error("解析 broker 管道安全描述符"));
        }
        let descriptor_guard = LocalGuard(descriptor as HLOCAL);
        let security = SECURITY_ATTRIBUTES {
            nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: descriptor,
            bInheritHandle: 0,
        };
        let name_wide = token::to_wide(&name);
        let pipe = unsafe {
            CreateNamedPipeW(
                name_wide.as_ptr(),
                PIPE_ACCESS_DUPLEX | FILE_FLAG_FIRST_PIPE_INSTANCE,
                PIPE_TYPE_MESSAGE | PIPE_READMODE_MESSAGE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS,
                1,
                PIPE_BUFFER,
                PIPE_BUFFER,
                0,
                &security,
            )
        };
        drop(descriptor_guard);
        if pipe == INVALID_HANDLE_VALUE {
            return Err(last_error("CreateNamedPipeW(broker)"));
        }
        let client = unsafe {
            CreateFileW(
                name_wide.as_ptr(),
                GENERIC_READ | GENERIC_WRITE,
                FILE_SHARE_READ | FILE_SHARE_WRITE,
                std::ptr::null(),
                OPEN_EXISTING,
                0,
                std::ptr::null_mut(),
            )
        };
        if client == INVALID_HANDLE_VALUE {
            drop(OwnedHandle(pipe));
            return Err(last_error("CreateFileW(broker connected client)"));
        }
        Ok(Self {
            pipe: OwnedHandle(pipe),
            client: OwnedHandle(client),
            appcontainer_sid: appcontainer_sid.to_string(),
            generation,
            nonce,
        })
    }

    pub(crate) fn client_handle(&self) -> HANDLE {
        self.client.raw()
    }

    pub(crate) fn generation(&self) -> u64 {
        self.generation
    }

    pub(crate) fn nonce(&self) -> [u8; 16] {
        self.nonce
    }

    pub(crate) fn register(self, process: HANDLE, job: &crate::job::Job) -> Result<BrokerSession> {
        let mut in_job = 0;
        if unsafe { IsProcessInJob(process, job.raw(), &mut in_job) } == 0 {
            return Err(last_error("IsProcessInJob(broker register)"));
        }
        if in_job == 0 {
            return Err(WboxError::spawn(
                "broker 拒绝注册：目标进程尚未加入容器 Job",
            ));
        }
        let actual_sid = process_appcontainer_sid_string(process)?;
        if !actual_sid.eq_ignore_ascii_case(&self.appcontainer_sid) {
            return Err(WboxError::spawn(format!(
                "broker 拒绝注册：目标 AppContainer SID 不匹配（期望 {}，实际 {}）",
                self.appcontainer_sid, actual_sid
            )));
        }
        let pid = unsafe { GetProcessId(process) };
        if pid == 0 {
            return Err(last_error("GetProcessId(broker register)"));
        }
        let mut owned_process = std::ptr::null_mut();
        let current = unsafe { GetCurrentProcess() };
        if unsafe {
            DuplicateHandle(
                current,
                process,
                current,
                &mut owned_process,
                0,
                0,
                DUPLICATE_SAME_ACCESS,
            )
        } == 0
        {
            return Err(last_error("DuplicateHandle(broker process)"));
        }
        Ok(BrokerSession {
            pipe: self.pipe,
            process: OwnedHandle(owned_process),
            pid,
            generation: self.generation,
            nonce: self.nonce,
        })
    }
}

pub(crate) struct BrokerSession {
    pipe: OwnedHandle,
    process: OwnedHandle,
    pid: u32,
    generation: u64,
    nonce: [u8; 16],
}

impl BrokerSession {
    pub(crate) fn serve_hello_ping(self) -> Result<()> {
        let connected = unsafe { ConnectNamedPipe(self.pipe.raw(), std::ptr::null_mut()) };
        if connected == 0 {
            let err = unsafe { GetLastError() };
            if err != ERROR_PIPE_CONNECTED {
                return Err(WboxError::spawn(format!(
                    "ConnectNamedPipe(broker) 失败，GetLastError={}",
                    err
                )));
            }
        }
        // The connected client HANDLE was inherited through HANDLE_LIST only after this exact
        // process passed Job + AppContainer SID registration.
        if unsafe { GetProcessId(self.process.raw()) } != self.pid {
            return Err(WboxError::spawn("broker 注册进程已失效"));
        }

        let hello = read_request(self.pipe.raw())?;
        let expected_hello = hello_payload(self.generation, self.nonce);
        let hello_status =
            if hello.opcode == OP_HELLO && hello.flags == 0 && hello.payload == expected_hello {
                STATUS_OK
            } else {
                STATUS_AUTH
            };
        write_response(
            self.pipe.raw(),
            &Response {
                opcode: hello.opcode,
                request_id: hello.request_id,
                status: hello_status,
                payload: Vec::new(),
            },
        )?;
        if hello_status != STATUS_OK {
            return Err(WboxError::spawn("broker HELLO 认证失败"));
        }

        let ping = read_request(self.pipe.raw())?;
        let ping_status = if ping.opcode == OP_PING && ping.flags == 0 && ping.payload.is_empty() {
            STATUS_OK
        } else {
            STATUS_PROTOCOL
        };
        write_response(
            self.pipe.raw(),
            &Response {
                opcode: ping.opcode,
                request_id: ping.request_id,
                status: ping_status,
                payload: Vec::new(),
            },
        )?;
        unsafe {
            FlushFileBuffers(self.pipe.raw());
            DisconnectNamedPipe(self.pipe.raw());
        }
        if ping_status == STATUS_OK {
            Ok(())
        } else {
            Err(WboxError::spawn("broker PING 帧无效"))
        }
    }
}

fn read_request(pipe: HANDLE) -> Result<Request> {
    let mut header = [0u8; REQUEST_HEADER_LEN];
    read_exact(pipe, &mut header)?;
    let payload_len = u32::from_le_bytes(header[16..20].try_into().unwrap()) as usize;
    if payload_len > MAX_PAYLOAD {
        return Err(WboxError::spawn(format!(
            "broker 请求 payload 超限：{} > {}",
            payload_len, MAX_PAYLOAD
        )));
    }
    let mut payload = vec![0u8; payload_len];
    if payload_len != 0 {
        read_exact(pipe, &mut payload)?;
    }
    Request::decode(&header, payload)
        .map_err(|e| WboxError::spawn(format!("broker 请求帧无效：{:?}", e)))
}

fn write_request(pipe: HANDLE, request: &Request) -> Result<()> {
    if request.payload.len() > MAX_PAYLOAD {
        return Err(WboxError::spawn("broker 请求 payload 超限"));
    }
    write_all(pipe, &request.encode_header())?;
    if !request.payload.is_empty() {
        write_all(pipe, &request.payload)?;
    }
    Ok(())
}

fn read_response(pipe: HANDLE) -> Result<Response> {
    let mut header = [0u8; RESPONSE_HEADER_LEN];
    read_exact(pipe, &mut header)?;
    let payload_len = u32::from_le_bytes(header[20..24].try_into().unwrap()) as usize;
    if payload_len > MAX_PAYLOAD {
        return Err(WboxError::spawn("broker 响应 payload 超限"));
    }
    let mut payload = vec![0u8; payload_len];
    if payload_len != 0 {
        read_exact(pipe, &mut payload)?;
    }
    Response::decode(&header, payload)
        .map_err(|e| WboxError::spawn(format!("broker 响应帧无效：{:?}", e)))
}

fn write_response(pipe: HANDLE, response: &Response) -> Result<()> {
    if response.payload.len() > MAX_PAYLOAD {
        return Err(WboxError::spawn("broker 响应 payload 超限"));
    }
    write_all(pipe, &response.encode_header())?;
    if !response.payload.is_empty() {
        write_all(pipe, &response.payload)?;
    }
    Ok(())
}

fn read_exact(handle: HANDLE, mut buffer: &mut [u8]) -> Result<()> {
    while !buffer.is_empty() {
        let mut read = 0;
        let ok = unsafe {
            ReadFile(
                handle,
                buffer.as_mut_ptr(),
                buffer.len() as u32,
                &mut read,
                std::ptr::null_mut(),
            )
        };
        if ok == 0 {
            let err = unsafe { GetLastError() };
            return Err(WboxError::spawn(format!(
                "ReadFile(broker) 失败，GetLastError={}",
                err
            )));
        }
        if read == 0 {
            return Err(WboxError::spawn("ReadFile(broker) 提前 EOF"));
        }
        buffer = &mut buffer[read as usize..];
    }
    Ok(())
}

fn write_all(handle: HANDLE, mut buffer: &[u8]) -> Result<()> {
    while !buffer.is_empty() {
        let mut written = 0;
        let ok = unsafe {
            WriteFile(
                handle,
                buffer.as_ptr(),
                buffer.len() as u32,
                &mut written,
                std::ptr::null_mut(),
            )
        };
        if ok == 0 {
            return Err(last_error("WriteFile(broker)"));
        }
        if written == 0 {
            return Err(WboxError::spawn("WriteFile(broker) 未产生进度"));
        }
        buffer = &buffer[written as usize..];
    }
    Ok(())
}

fn current_user_sid_string() -> Result<String> {
    let current = unsafe { GetCurrentProcess() };
    let mut token_handle = std::ptr::null_mut();
    if unsafe { OpenProcessToken(current, TOKEN_QUERY, &mut token_handle) } == 0 {
        return Err(last_error("OpenProcessToken(current user)"));
    }
    let token_handle = OwnedHandle(token_handle);
    let mut needed = 0;
    unsafe {
        GetTokenInformation(
            token_handle.raw(),
            TokenUser,
            std::ptr::null_mut(),
            0,
            &mut needed,
        )
    };
    if needed < std::mem::size_of::<TOKEN_USER>() as u32 {
        return Err(last_error("GetTokenInformation(TokenUser size)"));
    }
    let mut storage = vec![0usize; (needed as usize).div_ceil(std::mem::size_of::<usize>())];
    if unsafe {
        GetTokenInformation(
            token_handle.raw(),
            TokenUser,
            storage.as_mut_ptr().cast(),
            needed,
            &mut needed,
        )
    } == 0
    {
        return Err(last_error("GetTokenInformation(TokenUser)"));
    }
    let user = storage.as_ptr() as *const TOKEN_USER;
    token::sid_to_string(unsafe { (*user).User.Sid })
        .map_err(|e| WboxError::spawn(format!("转换当前用户 SID 失败：{}", e)))
}

fn process_appcontainer_sid_string(process: HANDLE) -> Result<String> {
    let mut token_handle = std::ptr::null_mut();
    if unsafe { OpenProcessToken(process, TOKEN_QUERY, &mut token_handle) } == 0 {
        return Err(last_error("OpenProcessToken(AppContainer client)"));
    }
    let token_handle = OwnedHandle(token_handle);
    let mut needed = 0;
    unsafe {
        GetTokenInformation(
            token_handle.raw(),
            TokenAppContainerSid,
            std::ptr::null_mut(),
            0,
            &mut needed,
        )
    };
    if needed < std::mem::size_of::<TOKEN_APPCONTAINER_INFORMATION>() as u32 {
        return Err(last_error("GetTokenInformation(TokenAppContainerSid size)"));
    }
    let mut storage = vec![0usize; (needed as usize).div_ceil(std::mem::size_of::<usize>())];
    if unsafe {
        GetTokenInformation(
            token_handle.raw(),
            TokenAppContainerSid,
            storage.as_mut_ptr().cast(),
            needed,
            &mut needed,
        )
    } == 0
    {
        return Err(last_error("GetTokenInformation(TokenAppContainerSid)"));
    }
    let info = storage.as_ptr() as *const TOKEN_APPCONTAINER_INFORMATION;
    let sid = unsafe { (*info).TokenAppContainer };
    if sid.is_null() {
        return Err(WboxError::spawn(
            "broker 拒绝注册：目标进程不是 AppContainer",
        ));
    }
    token::sid_to_string(sid)
        .map_err(|e| WboxError::spawn(format!("转换 AppContainer SID 失败：{}", e)))
}

fn hello_payload(generation: u64, nonce: [u8; 16]) -> Vec<u8> {
    let mut payload = Vec::with_capacity(24);
    payload.extend_from_slice(&generation.to_le_bytes());
    payload.extend_from_slice(&nonce);
    payload
}

fn hex(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        use std::fmt::Write;
        let _ = write!(out, "{:02x}", byte);
    }
    out
}

fn last_error(context: &str) -> WboxError {
    WboxError::spawn(format!("{} 失败，GetLastError={}", context, unsafe {
        GetLastError()
    }))
}

struct LocalGuard(HLOCAL);

impl Drop for LocalGuard {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe { LocalFree(self.0) };
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_codec_round_trip_and_limits() {
        let request = Request {
            opcode: OP_PING,
            request_id: 42,
            flags: 7,
            payload: b"abc".to_vec(),
        };
        let header = request.encode_header();
        assert_eq!(Request::decode(&header, b"abc".to_vec()).unwrap(), request);

        let mut oversized = header;
        oversized[16..20].copy_from_slice(&((MAX_PAYLOAD + 1) as u32).to_le_bytes());
        assert_eq!(
            Request::decode(&oversized, Vec::new()),
            Err(DecodeError::PayloadTooLarge)
        );
        let mut wrong_version = header;
        wrong_version[4..6].copy_from_slice(&(VERSION + 1).to_le_bytes());
        assert_eq!(
            Request::decode(&wrong_version, b"abc".to_vec()),
            Err(DecodeError::Version)
        );
    }

    #[test]
    fn response_codec_round_trip() {
        let response = Response {
            opcode: OP_HELLO,
            request_id: u64::MAX,
            status: STATUS_AUTH,
            payload: vec![1, 2, 3],
        };
        assert_eq!(
            Response::decode(&response.encode_header(), response.payload.clone()).unwrap(),
            response
        );
    }

    fn open_request(path: &[u8], mount_id: u32, flags: u32, mode: u32) -> Request {
        let mut payload = Vec::with_capacity(OPEN_FIXED_LEN + path.len());
        payload.extend_from_slice(&mount_id.to_le_bytes());
        payload.extend_from_slice(&flags.to_le_bytes());
        payload.extend_from_slice(&mode.to_le_bytes());
        payload.extend_from_slice(path);
        Request {
            opcode: OP_OPEN,
            request_id: 9,
            flags: 0,
            payload,
        }
    }

    #[test]
    fn open_request_accepts_only_normalized_read_only_relative_paths() {
        let parsed = OpenRequest::decode(&open_request(b"dir/canary.txt", 7, 0, 0)).unwrap();
        assert_eq!(parsed.mount_id, 7);
        assert_eq!(parsed.components, ["dir", "canary.txt"]);

        for path in [
            b"".as_slice(),
            b"/absolute",
            b"../escape",
            b"dir/../escape",
            b"dir/./file",
            b"dir//file",
            b"dir\\file",
            b"C:drive",
        ] {
            assert!(
                OpenRequest::decode(&open_request(path, 7, 0, 0)).is_err(),
                "must reject {:?}",
                String::from_utf8_lossy(path)
            );
        }
        assert!(OpenRequest::decode(&open_request(b"file", 0, 0, 0)).is_err());
        assert!(OpenRequest::decode(&open_request(b"file", 7, 1, 0)).is_err());
        assert!(OpenRequest::decode(&open_request(b"file", 7, 0, 0o644)).is_err());
        assert!(
            OpenRequest::decode(&open_request(&vec![b'a'; MAX_RELATIVE_PATH + 1], 7, 0, 0))
                .is_err()
        );
        assert!(OpenRequest::decode(&open_request(&[0xff], 7, 0, 0)).is_err());
    }

    #[test]
    fn broker_child_probe() {
        let Ok(raw_handle) = std::env::var("WBOX_TEST_BROKER_HANDLE") else {
            return;
        };
        let generation = std::env::var("WBOX_TEST_BROKER_GENERATION")
            .unwrap()
            .parse::<u64>()
            .unwrap();
        let nonce_hex = std::env::var("WBOX_TEST_BROKER_NONCE").unwrap();
        let mut nonce = [0u8; 16];
        for (index, byte) in nonce.iter_mut().enumerate() {
            *byte = u8::from_str_radix(&nonce_hex[index * 2..index * 2 + 2], 16).unwrap();
        }
        let pipe = OwnedHandle(raw_handle.parse::<usize>().unwrap() as HANDLE);
        write_request(
            pipe.raw(),
            &Request {
                opcode: OP_HELLO,
                request_id: 1,
                flags: 0,
                payload: hello_payload(generation, nonce),
            },
        )
        .unwrap();
        let hello = read_response(pipe.raw()).unwrap();
        assert_eq!(hello.status, STATUS_OK);
        assert_eq!(hello.request_id, 1);

        write_request(
            pipe.raw(),
            &Request {
                opcode: OP_PING,
                request_id: 2,
                flags: 0,
                payload: Vec::new(),
            },
        )
        .unwrap();
        let ping = read_response(pipe.raw()).unwrap();
        assert_eq!(ping.status, STATUS_OK);
        assert_eq!(ping.request_id, 2);
    }

    #[test]
    fn appcontainer_hello_ping_is_pid_sid_and_job_bound() {
        use crate::backend::Limits;
        use crate::sandbox;
        use crate::token::AppContainerProfile;

        fn unique(label: &str) -> String {
            format!(
                "wbox_broker_{}_{}_{}",
                std::process::id(),
                label,
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            )
        }

        let base = std::env::temp_dir().join(unique("probe"));
        std::fs::create_dir_all(&base).unwrap();
        let child = base.join("wbox-broker-probe.exe");
        std::fs::copy(std::env::current_exe().unwrap(), &child).unwrap();
        crate::acl::grant_read_recursive(&base).unwrap();

        let profile = AppContainerProfile::create(&unique("profile"), &[]).unwrap();
        let endpoint = BrokerEndpoint::create(&profile.sid_string().unwrap()).unwrap();
        let mut env = crate::backend::env::build_child_env(
            &[],
            &[],
            false,
            crate::backend::env::GuestFlavor::Windows,
        );
        env.push((
            "WBOX_TEST_BROKER_HANDLE".to_string(),
            (endpoint.client_handle() as usize).to_string(),
        ));
        env.push((
            "WBOX_TEST_BROKER_GENERATION".to_string(),
            endpoint.generation().to_string(),
        ));
        env.push(("WBOX_TEST_BROKER_NONCE".to_string(), hex(&endpoint.nonce())));
        let cmdline = sandbox::build_cmdline(&[
            child.to_string_lossy().into_owned(),
            "--exact".to_string(),
            "broker::tests::broker_child_probe".to_string(),
            "--nocapture".to_string(),
        ])
        .unwrap();
        let mut job = crate::job::Job::create(Limits::default()).unwrap();
        let mut server = None;
        let client_handle = endpoint.client_handle();
        let rc = sandbox::run_container_with_handles_and_created_hook(
            &profile,
            &[],
            &cmdline,
            &base.to_string_lossy(),
            &mut job,
            &env,
            &[client_handle],
            |process, assigned_job| {
                let session = endpoint.register(process, assigned_job)?;
                server = Some(std::thread::spawn(move || session.serve_hello_ping()));
                Ok(())
            },
        )
        .unwrap();
        let server_result = server
            .expect("on_created 必须启动 broker server")
            .join()
            .expect("broker server panic");
        assert_eq!(
            rc, 0,
            "AppContainer broker child probe 失败；server={:?}",
            server_result
        );
        server_result.unwrap();

        drop(profile);
        let _ = std::fs::remove_dir_all(base);
    }

    #[test]
    fn register_rejects_process_outside_target_job() {
        use crate::backend::Limits;
        use crate::token::AppContainerProfile;

        let profile_name = format!("wbox_broker_job_{}", std::process::id());
        let profile = AppContainerProfile::create(&profile_name, &[]).unwrap();
        let endpoint = BrokerEndpoint::create(&profile.sid_string().unwrap()).unwrap();
        let job = crate::job::Job::create(Limits::default()).unwrap();
        let err = endpoint
            .register(unsafe { GetCurrentProcess() }, &job)
            .err()
            .expect("当前测试进程不在新建 Job 中，必须拒绝");
        assert!(format!("{}", err).contains("Job"), "{}", err);
    }

    #[test]
    fn register_rejects_different_appcontainer_sid() {
        use crate::backend::Limits;
        use crate::sandbox;
        use crate::token::AppContainerProfile;

        let tag = format!(
            "{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        );
        let expected =
            AppContainerProfile::create(&format!("wbox_broker_expected_{}", tag), &[]).unwrap();
        let actual =
            AppContainerProfile::create(&format!("wbox_broker_actual_{}", tag), &[]).unwrap();
        let endpoint = BrokerEndpoint::create(&expected.sid_string().unwrap()).unwrap();
        let exe = r"C:\Windows\System32\hostname.exe";
        assert!(std::path::Path::new(exe).is_file());
        let cmdline = sandbox::build_cmdline(&[exe.to_string()]).unwrap();
        let env = crate::backend::env::build_child_env(
            &[],
            &[],
            false,
            crate::backend::env::GuestFlavor::Windows,
        );
        let mut job = crate::job::Job::create(Limits::default()).unwrap();
        let err = sandbox::run_container_with_handles_and_created_hook(
            &actual,
            &[],
            &cmdline,
            r"C:\Windows\System32",
            &mut job,
            &env,
            &[],
            |process, assigned_job| endpoint.register(process, assigned_job).map(|_| ()),
        )
        .unwrap_err();
        assert!(format!("{}", err).contains("SID 不匹配"), "{}", err);
    }
}
