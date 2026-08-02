//! Windows OCI filesystem broker transport.
//!
//! The transport exposes authenticated HELLO/PING plus read-only filesystem OPEN after
//! component-wise reparse rejection and fd-backed hostfs product gates.

use crate::error::{Result, WboxError};
use std::io::Write as _;
#[cfg(test)]
use std::os::windows::io::RawHandle;
use std::os::windows::io::{AsHandle as _, AsRawHandle as _};
use std::time::Duration;
use windows_sys::Win32::Foundation::HANDLE;
#[cfg(test)]
use windows_sys::Win32::System::Threading::GetCurrentProcess;

const MAGIC: u32 = 0x5742_4f58; // "WBOX"
const VERSION: u16 = 1;
const REQUEST_HEADER_LEN: usize = 24;
const RESPONSE_HEADER_LEN: usize = 24;
const MAX_PAYLOAD: usize = 4096;
const BROKER_IO_TIMEOUT: Duration = Duration::from_secs(30);

const OP_HELLO: u16 = 1;
const OP_PING: u16 = 2;
const OP_OPEN: u16 = 3;
const OPEN_FIXED_LEN: usize = 12;
const MAX_RELATIVE_PATH: usize = 1024;

const STATUS_OK: i32 = 0;
const STATUS_PROTOCOL: i32 = -71; // Linux EPROTO
const STATUS_AUTH: i32 = -13; // Linux EACCES
const STATUS_NOT_FOUND: i32 = -2; // Linux ENOENT

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

pub(crate) struct BrokerMount {
    id: u32,
    root: std::fs::File,
}

impl BrokerMount {
    pub(crate) fn open_readonly(id: u32, path: &std::path::Path) -> Result<Self> {
        if id == 0 {
            return Err(WboxError::args("broker mount id 必须非零"));
        }
        let root = agenterm_platform::filesystem_open::open_existing(
            path,
            agenterm_platform::filesystem_open::ExistingEntryType::Directory,
        )
        .map_err(|error| {
            WboxError::args(format!(
                "broker mount 根必须是无 reparse 的真实目录：{error}"
            ))
        })?;
        Ok(Self { id, root })
    }

    fn open_existing_file(&self, components: &[String]) -> Result<std::fs::File> {
        let mut parent: Option<std::fs::File> = None;
        for (index, component) in components.iter().enumerate() {
            let root = parent.as_ref().unwrap_or(&self.root);
            let final_component = index + 1 == components.len();
            let expected = if final_component {
                agenterm_platform::filesystem_open::ExistingEntryType::File
            } else {
                agenterm_platform::filesystem_open::ExistingEntryType::Directory
            };
            let opened = agenterm_platform::filesystem_open::open_existing_child(
                root,
                std::ffi::OsStr::new(component),
                expected,
            )
            .map_err(|error| {
                WboxError::spawn(format!(
                    "broker 拒绝 reparse 或类型不符的 path component '{}': {error}",
                    component
                ))
            })?;
            if final_component {
                return Ok(opened);
            }
            parent = Some(opened);
        }
        Err(WboxError::spawn("broker OPEN 缺少路径组件"))
    }
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
    pipe: agenterm_platform::ipc::NativeStream,
    client: agenterm_platform::ipc::NativeStream,
    appcontainer_sid: String,
    generation: u64,
    nonce: [u8; 16],
    mounts: Vec<BrokerMount>,
}

impl BrokerEndpoint {
    pub(crate) fn create(appcontainer_sid: &str) -> Result<Self> {
        Self::create_with_mounts(appcontainer_sid, Vec::new())
    }

    pub(crate) fn create_with_mounts(
        appcontainer_sid: &str,
        mounts: Vec<BrokerMount>,
    ) -> Result<Self> {
        let mut ids = mounts.iter().map(|mount| mount.id).collect::<Vec<_>>();
        ids.sort_unstable();
        if ids.windows(2).any(|pair| pair[0] == pair[1]) {
            return Err(WboxError::args("broker mount id 不能重复"));
        }
        let random = agenterm_platform::entropy::secure_random_array::<32>().map_err(|error| {
            WboxError::spawn(format!("生成 broker endpoint 随机身份失败：{error}"))
        })?;
        let generation = u64::from_le_bytes(random[0..8].try_into().unwrap());
        let nonce: [u8; 16] = random[8..24].try_into().unwrap();
        let suffix = hex(&random[24..32]);
        let name = format!(
            r"\\.\pipe\wbox.{}.{}.{}",
            std::process::id(),
            generation,
            suffix
        );
        let endpoint = agenterm_platform::ipc::IpcEndpoint::NamedPipe(name);
        let mut listener = agenterm_platform::ipc::NativeListener::bind(&endpoint)
            .map_err(|error| WboxError::spawn(format!("创建 broker 本地 IPC 失败：{error}")))?;
        // Guest never opens this random owner-only endpoint. The supervisor connects both
        // ends before spawn and inherits only the connected client HANDLE.
        let client = agenterm_platform::ipc::NativeStream::connect(&endpoint, BROKER_IO_TIMEOUT)
            .map_err(|error| WboxError::spawn(format!("连接 broker 本地 IPC 失败：{error}")))?;
        let pipe = listener
            .accept(BROKER_IO_TIMEOUT)
            .map_err(|error| WboxError::spawn(format!("接受 broker 本地 IPC 失败：{error}")))?;
        Ok(Self {
            pipe,
            client,
            appcontainer_sid: appcontainer_sid.to_string(),
            generation,
            nonce,
            mounts,
        })
    }

    pub(crate) fn client_handle(&self) -> HANDLE {
        self.client.as_raw_handle() as HANDLE
    }

    pub(crate) fn generation(&self) -> u64 {
        self.generation
    }

    pub(crate) fn nonce(&self) -> [u8; 16] {
        self.nonce
    }

    pub(crate) fn register(
        self,
        process: &agenterm_platform::process_reference::ProcessReference,
        job: &crate::job::Job,
    ) -> Result<BrokerSession> {
        let process = agenterm_platform::process_reference::ProcessReference::duplicate_from(
            process.as_handle(),
        )
        .map_err(|error| WboxError::spawn(format!("保留 broker 进程引用失败：{error}")))?;
        let in_job = job.contains(&process)?;
        if !in_job {
            return Err(WboxError::spawn(
                "broker 拒绝注册：目标进程尚未加入容器 Job",
            ));
        }
        let actual_sid = process_appcontainer_sid_string(&process)?;
        if !actual_sid.eq_ignore_ascii_case(&self.appcontainer_sid) {
            return Err(WboxError::spawn(format!(
                "broker 拒绝注册：目标 AppContainer SID 不匹配（期望 {}，实际 {}）",
                self.appcontainer_sid, actual_sid
            )));
        }
        Ok(BrokerSession {
            pipe: self.pipe,
            process,
            generation: self.generation,
            nonce: self.nonce,
            mounts: self.mounts,
        })
    }
}

pub(crate) struct BrokerSession {
    pipe: agenterm_platform::ipc::NativeStream,
    process: agenterm_platform::process_reference::ProcessReference,
    generation: u64,
    nonce: [u8; 16],
    mounts: Vec<BrokerMount>,
}

impl BrokerSession {
    pub(crate) fn serve_hello_ping(self) -> Result<()> {
        self.serve(false)
    }

    pub(crate) fn serve_hello_ping_open(self) -> Result<()> {
        self.serve(true)
    }

    fn serve(mut self, expect_open: bool) -> Result<()> {
        // The connected client HANDLE was inherited through HANDLE_LIST only after this exact
        // process passed Job + AppContainer SID registration.
        if !self
            .process
            .is_alive()
            .map_err(|error| WboxError::spawn(format!("检查 broker 进程引用失败：{error}")))?
        {
            return Err(WboxError::spawn("broker 注册进程已失效"));
        }

        let hello = read_request(&mut self.pipe)?;
        let expected_hello = hello_payload(self.generation, self.nonce);
        let hello_status =
            if hello.opcode == OP_HELLO && hello.flags == 0 && hello.payload == expected_hello {
                STATUS_OK
            } else {
                STATUS_AUTH
            };
        write_response(
            &mut self.pipe,
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

        let ping = read_request(&mut self.pipe)?;
        let ping_status = if ping.opcode == OP_PING && ping.flags == 0 && ping.payload.is_empty() {
            STATUS_OK
        } else {
            STATUS_PROTOCOL
        };
        write_response(
            &mut self.pipe,
            &Response {
                opcode: ping.opcode,
                request_id: ping.request_id,
                status: ping_status,
                payload: Vec::new(),
            },
        )?;
        if ping_status == STATUS_OK && expect_open {
            self.serve_open()?;
        }
        self.pipe
            .flush()
            .map_err(|error| WboxError::spawn(format!("刷新 broker 本地 IPC 失败：{error}")))?;
        if ping_status == STATUS_OK {
            Ok(())
        } else {
            Err(WboxError::spawn("broker PING 帧无效"))
        }
    }

    fn serve_open(&mut self) -> Result<()> {
        let request = read_request(&mut self.pipe)?;
        let request_id = request.request_id;
        let opcode = request.opcode;
        let parsed = match OpenRequest::decode(&request) {
            Ok(parsed) => parsed,
            Err(_) => {
                return write_response(
                    &mut self.pipe,
                    &Response {
                        opcode,
                        request_id,
                        status: STATUS_PROTOCOL,
                        payload: Vec::new(),
                    },
                );
            }
        };
        let Some(mount) = self.mounts.iter().find(|mount| mount.id == parsed.mount_id) else {
            return write_response(
                &mut self.pipe,
                &Response {
                    opcode,
                    request_id,
                    status: STATUS_NOT_FOUND,
                    payload: Vec::new(),
                },
            );
        };
        let opened = match mount.open_existing_file(&parsed.components) {
            Ok(opened) => opened,
            Err(_) => {
                return write_response(
                    &mut self.pipe,
                    &Response {
                        opcode,
                        request_id,
                        status: STATUS_AUTH,
                        payload: Vec::new(),
                    },
                );
            }
        };
        let remote =
            agenterm_platform::adapters::windows::process_reference::duplicate_handle_into(
                &self.process,
                opened.as_handle(),
            )
            .map_err(|error| {
                WboxError::spawn(format!("向 broker 目标进程复制文件 HANDLE 失败：{error}"))
            })?;
        let payload = (remote.as_raw_handle() as usize as u64)
            .to_le_bytes()
            .to_vec();
        write_response(
            &mut self.pipe,
            &Response {
                opcode,
                request_id,
                status: STATUS_OK,
                payload,
            },
        )?;
        // The target owns the HANDLE only after its numeric value was delivered.
        // A failed response write drops `remote` and rolls the target HANDLE back.
        let _remote = remote.into_raw_handle();
        Ok(())
    }
}

fn read_request(pipe: &mut impl std::io::Read) -> Result<Request> {
    let mut header = [0u8; REQUEST_HEADER_LEN];
    pipe.read_exact(&mut header)
        .map_err(|error| WboxError::spawn(format!("读取 broker 请求头失败：{error}")))?;
    let payload_len = u32::from_le_bytes(header[16..20].try_into().unwrap()) as usize;
    if payload_len > MAX_PAYLOAD {
        return Err(WboxError::spawn(format!(
            "broker 请求 payload 超限：{} > {}",
            payload_len, MAX_PAYLOAD
        )));
    }
    let mut payload = vec![0u8; payload_len];
    if payload_len != 0 {
        pipe.read_exact(&mut payload)
            .map_err(|error| WboxError::spawn(format!("读取 broker 请求体失败：{error}")))?;
    }
    Request::decode(&header, payload)
        .map_err(|e| WboxError::spawn(format!("broker 请求帧无效：{:?}", e)))
}

fn write_request(pipe: &mut impl std::io::Write, request: &Request) -> Result<()> {
    if request.payload.len() > MAX_PAYLOAD {
        return Err(WboxError::spawn("broker 请求 payload 超限"));
    }
    pipe.write_all(&request.encode_header())
        .map_err(|error| WboxError::spawn(format!("写入 broker 请求头失败：{error}")))?;
    if !request.payload.is_empty() {
        pipe.write_all(&request.payload)
            .map_err(|error| WboxError::spawn(format!("写入 broker 请求体失败：{error}")))?;
    }
    Ok(())
}

fn read_response(pipe: &mut impl std::io::Read) -> Result<Response> {
    let mut header = [0u8; RESPONSE_HEADER_LEN];
    pipe.read_exact(&mut header)
        .map_err(|error| WboxError::spawn(format!("读取 broker 响应头失败：{error}")))?;
    let payload_len = u32::from_le_bytes(header[20..24].try_into().unwrap()) as usize;
    if payload_len > MAX_PAYLOAD {
        return Err(WboxError::spawn("broker 响应 payload 超限"));
    }
    let mut payload = vec![0u8; payload_len];
    if payload_len != 0 {
        pipe.read_exact(&mut payload)
            .map_err(|error| WboxError::spawn(format!("读取 broker 响应体失败：{error}")))?;
    }
    Response::decode(&header, payload)
        .map_err(|e| WboxError::spawn(format!("broker 响应帧无效：{:?}", e)))
}

fn write_response(pipe: &mut impl std::io::Write, response: &Response) -> Result<()> {
    if response.payload.len() > MAX_PAYLOAD {
        return Err(WboxError::spawn("broker 响应 payload 超限"));
    }
    pipe.write_all(&response.encode_header())
        .map_err(|error| WboxError::spawn(format!("写入 broker 响应头失败：{error}")))?;
    if !response.payload.is_empty() {
        pipe.write_all(&response.payload)
            .map_err(|error| WboxError::spawn(format!("写入 broker 响应体失败：{error}")))?;
    }
    Ok(())
}

fn process_appcontainer_sid_string(
    process: &agenterm_platform::process_reference::ProcessReference,
) -> Result<String> {
    let facts = agenterm_platform::process_security::process_handle(process.as_handle())
        .map_err(|error| WboxError::spawn(format!("读取 broker 进程安全身份失败：{error}")))?;
    let sid = facts
        .windows_app_container_sid()
        .ok_or_else(|| WboxError::spawn("broker 拒绝注册：目标进程不是 AppContainer"))?;
    agenterm_platform::adapters::windows::app_container::sid_string(sid)
        .map_err(|error| WboxError::spawn(format!("转换 AppContainer SID 失败：{error}")))
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
    fn readonly_mount_rejects_intermediate_junction_escape() {
        let tag = format!(
            "{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        );
        let root = std::env::temp_dir().join(format!("wbox_broker_root_{}", tag));
        let root_link = std::env::temp_dir().join(format!("wbox_broker_root_link_{}", tag));
        let outside = std::env::temp_dir().join(format!("wbox_broker_outside_{}", tag));
        let jump = root.join("jump");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(outside.join("canary.txt"), b"outside").unwrap();
        let output = std::process::Command::new("cmd.exe")
            .args(["/d", "/c", "mklink", "/J"])
            .arg(&jump)
            .arg(&outside)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "mklink /J failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );

        let output = std::process::Command::new("cmd.exe")
            .args(["/d", "/c", "mklink", "/J"])
            .arg(&root_link)
            .arg(&root)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "root mklink /J failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let error = match BrokerMount::open_readonly(2, &root_link) {
            Ok(_) => panic!("junction mount root must be rejected"),
            Err(error) => error,
        };
        assert!(error.to_string().contains("reparse"), "{error}");

        let mount = BrokerMount::open_readonly(1, &root).unwrap();
        let err = match mount.open_existing_file(&["jump".to_string(), "canary.txt".to_string()]) {
            Ok(_) => panic!("junction component must be rejected"),
            Err(err) => err,
        };
        assert!(format!("{}", err).contains("reparse"), "{}", err);
        assert_eq!(
            std::fs::read(outside.join("canary.txt")).unwrap(),
            b"outside"
        );

        std::fs::remove_dir(&jump).unwrap();
        std::fs::remove_dir(&root_link).unwrap();
        std::fs::remove_dir_all(root).unwrap();
        std::fs::remove_dir_all(outside).unwrap();
    }

    #[test]
    fn broker_child_probe() {
        use agenterm_platform::ipc::NativeStreamExt as _;

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
        use std::os::windows::io::FromRawHandle as _;
        let owned = unsafe {
            std::os::windows::io::OwnedHandle::from_raw_handle(
                raw_handle.parse::<usize>().unwrap() as _
            )
        };
        let mut pipe =
            agenterm_platform::ipc::NativeStream::from_owned_handle(owned, BROKER_IO_TIMEOUT);
        write_request(
            &mut pipe,
            &Request {
                opcode: OP_HELLO,
                request_id: 1,
                flags: 0,
                payload: hello_payload(generation, nonce),
            },
        )
        .unwrap();
        let hello = read_response(&mut pipe).unwrap();
        assert_eq!(hello.status, STATUS_OK);
        assert_eq!(hello.request_id, 1);

        write_request(
            &mut pipe,
            &Request {
                opcode: OP_PING,
                request_id: 2,
                flags: 0,
                payload: Vec::new(),
            },
        )
        .unwrap();
        let ping = read_response(&mut pipe).unwrap();
        assert_eq!(ping.status, STATUS_OK);
        assert_eq!(ping.request_id, 2);

        if let Ok(path) = std::env::var("WBOX_TEST_BROKER_OPEN_PATH") {
            use windows_sys::Win32::Storage::FileSystem::ReadFile;

            write_request(&mut pipe, &open_request(path.as_bytes(), 1, 0, 0)).unwrap();
            let opened = read_response(&mut pipe).unwrap();
            assert_eq!(opened.status, STATUS_OK, "broker OPEN failed");
            assert_eq!(opened.payload.len(), 8);
            let remote = u64::from_le_bytes(opened.payload.try_into().unwrap()) as usize as HANDLE;
            let remote =
                unsafe { std::os::windows::io::OwnedHandle::from_raw_handle(remote as RawHandle) };
            let mut buffer = [0u8; 64];
            let mut read = 0;
            assert_ne!(
                unsafe {
                    ReadFile(
                        remote.as_raw_handle() as HANDLE,
                        buffer.as_mut_ptr(),
                        buffer.len() as u32,
                        &mut read,
                        std::ptr::null_mut(),
                    )
                },
                0
            );
            assert_eq!(
                &buffer[..read as usize],
                std::env::var("WBOX_TEST_BROKER_OPEN_EXPECTED")
                    .unwrap()
                    .as_bytes()
            );
        }
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
        let volume = std::env::temp_dir().join(unique("volume"));
        std::fs::create_dir_all(&base).unwrap();
        std::fs::create_dir_all(volume.join("nested")).unwrap();
        std::fs::write(volume.join("nested").join("canary.txt"), b"broker-canary").unwrap();
        let child = base.join("wbox-broker-probe.exe");
        std::fs::copy(std::env::current_exe().unwrap(), &child).unwrap();
        crate::acl::grant_read_recursive(&base).unwrap();

        let profile = AppContainerProfile::create(&unique("profile"), &[]).unwrap();
        let mount = BrokerMount::open_readonly(1, &volume).unwrap();
        let endpoint =
            BrokerEndpoint::create_with_mounts(&profile.sid_string().unwrap(), vec![mount])
                .unwrap();
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
        env.push((
            "WBOX_TEST_BROKER_OPEN_PATH".to_string(),
            "nested/canary.txt".to_string(),
        ));
        env.push((
            "WBOX_TEST_BROKER_OPEN_EXPECTED".to_string(),
            "broker-canary".to_string(),
        ));
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
                server = Some(std::thread::spawn(move || session.serve_hello_ping_open()));
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
        let _ = std::fs::remove_dir_all(volume);
    }

    #[test]
    fn register_rejects_process_outside_target_job() {
        use crate::backend::Limits;
        use crate::token::AppContainerProfile;

        let profile_name = format!("wbox_broker_job_{}", std::process::id());
        let profile = AppContainerProfile::create(&profile_name, &[]).unwrap();
        let endpoint = BrokerEndpoint::create(&profile.sid_string().unwrap()).unwrap();
        let job = crate::job::Job::create(Limits::default()).unwrap();
        let current = unsafe {
            std::os::windows::io::BorrowedHandle::borrow_raw(GetCurrentProcess() as RawHandle)
        };
        let process =
            agenterm_platform::process_reference::ProcessReference::duplicate_from(current)
                .unwrap();
        let err = endpoint
            .register(&process, &job)
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
