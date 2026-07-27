//! Windows OCI filesystem broker transport.
//!
//! This module deliberately exposes only HELLO/PING. Filesystem OPEN is added only after
//! component-wise reparse rejection and fd-backed hostfs have product gates.

use crate::error::{Result, WboxError};
use crate::token::{self, OwnedHandle};
use windows_sys::Win32::Foundation::{
    DuplicateHandle, GetLastError, LocalFree, DUPLICATE_CLOSE_SOURCE, DUPLICATE_SAME_ACCESS,
    ERROR_BROKEN_PIPE, ERROR_NO_DATA, ERROR_PIPE_CONNECTED, GENERIC_READ, GENERIC_WRITE, HANDLE,
    HLOCAL, INVALID_HANDLE_VALUE,
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
    CreateFileW, FileAttributeTagInfo, FlushFileBuffers, GetFileInformationByHandle,
    GetFileInformationByHandleEx, ReadFile, WriteFile, BY_HANDLE_FILE_INFORMATION,
    FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_REPARSE_POINT, FILE_ATTRIBUTE_TAG_INFO,
    FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_FIRST_PIPE_INSTANCE, FILE_FLAG_OPEN_REPARSE_POINT,
    FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING, PIPE_ACCESS_DUPLEX,
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
const OP_LOOKUP: u16 = 4;
const OP_READDIR: u16 = 5;
const OPEN_FIXED_LEN: usize = 12;
const READDIR_FIXED_LEN: usize = 8;
const MAX_RELATIVE_PATH: usize = 1024;
const MAX_DIRECTORY_ENTRIES: usize = 65_536;
const MAX_DIRECTORY_NAME_BYTES: usize = 16 * 1024 * 1024;

const STATUS_OK: i32 = 0;
const STATUS_PROTOCOL: i32 = -71; // Linux EPROTO
const STATUS_AUTH: i32 = -13; // Linux EACCES
const STATUS_NOT_FOUND: i32 = -2; // Linux ENOENT

const FILE_LIST_DIRECTORY: u32 = 0x0001;
const FILE_READ_DATA: u32 = 0x0001;
const FILE_READ_ATTRIBUTES: u32 = 0x0080;
const SYNCHRONIZE_ACCESS: u32 = 0x0010_0000;
const OBJ_CASE_INSENSITIVE: u32 = 0x0040;
const FILE_OPEN: u32 = 1;
const FILE_DIRECTORY_FILE: u32 = 0x0001;
const FILE_NON_DIRECTORY_FILE: u32 = 0x0040;
const FILE_SYNCHRONOUS_IO_NONALERT: u32 = 0x0020;
const FILE_OPEN_REPARSE_POINT_OPTION: u32 = 0x0020_0000;
const FILE_NAMES_INFORMATION_CLASS: u32 = 12;
const STATUS_NO_MORE_FILES_NT: i32 = 0x8000_0006u32 as i32;
const STATUS_BUFFER_OVERFLOW_NT: i32 = 0x8000_0005u32 as i32;

#[repr(C)]
struct NtUnicodeString {
    length: u16,
    maximum_length: u16,
    buffer: *mut u16,
}

#[repr(C)]
struct NtObjectAttributes {
    length: u32,
    root_directory: HANDLE,
    object_name: *mut NtUnicodeString,
    attributes: u32,
    security_descriptor: *mut core::ffi::c_void,
    security_quality_of_service: *mut core::ffi::c_void,
}

#[repr(C)]
struct NtIoStatusBlock {
    status: isize,
    information: usize,
}

#[link(name = "ntdll")]
extern "system" {
    fn NtCreateFile(
        file_handle: *mut HANDLE,
        desired_access: u32,
        object_attributes: *mut NtObjectAttributes,
        io_status_block: *mut NtIoStatusBlock,
        allocation_size: *mut i64,
        file_attributes: u32,
        share_access: u32,
        create_disposition: u32,
        create_options: u32,
        ea_buffer: *mut core::ffi::c_void,
        ea_length: u32,
    ) -> i32;

    fn NtQueryDirectoryFile(
        file_handle: HANDLE,
        event: HANDLE,
        apc_routine: *mut core::ffi::c_void,
        apc_context: *mut core::ffi::c_void,
        io_status_block: *mut NtIoStatusBlock,
        file_information: *mut core::ffi::c_void,
        length: u32,
        file_information_class: u32,
        return_single_entry: u8,
        file_name: *mut NtUnicodeString,
        restart_scan: u8,
    ) -> i32;
}

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

#[derive(Debug, Clone, PartialEq, Eq)]
struct PathRequest {
    mount_id: u32,
    components: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ReadDirRequest {
    path: PathRequest,
    cursor: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct BrokerMetadata {
    mode: u32,
    size: u64,
    inode: u64,
}

#[derive(Debug, Clone, Copy)]
enum FinalKind {
    File,
    Directory,
    Any,
}

pub(crate) struct BrokerMount {
    id: u32,
    root: OwnedHandle,
}

impl BrokerMount {
    pub(crate) fn open_readonly(id: u32, path: &std::path::Path) -> Result<Self> {
        if id == 0 {
            return Err(WboxError::args("broker mount id 必须非零"));
        }
        let path_wide = token::to_wide(&path.to_string_lossy());
        let root = unsafe {
            CreateFileW(
                path_wide.as_ptr(),
                FILE_LIST_DIRECTORY | FILE_READ_ATTRIBUTES | SYNCHRONIZE_ACCESS,
                FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
                std::ptr::null(),
                OPEN_EXISTING,
                FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT,
                std::ptr::null_mut(),
            )
        };
        if root == INVALID_HANDLE_VALUE {
            return Err(last_error("打开 broker mount 根目录"));
        }
        let root = OwnedHandle(root);
        let attributes = file_attributes(root.raw())?;
        if attributes & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
            return Err(WboxError::args("broker mount 根目录不能是 reparse point"));
        }
        if attributes & FILE_ATTRIBUTE_DIRECTORY == 0 {
            return Err(WboxError::args("broker mount 根必须是目录"));
        }
        Ok(Self { id, root })
    }

    fn open_existing_file(&self, components: &[String]) -> Result<OwnedHandle> {
        self.open_path(components, FinalKind::File)
    }

    fn lookup(&self, components: &[String]) -> Result<BrokerMetadata> {
        if components.is_empty() {
            return metadata_from_handle(self.root.raw());
        }
        let opened = self.open_path(components, FinalKind::Any)?;
        metadata_from_handle(opened.raw())
    }

    fn read_directory(&self, components: &[String]) -> Result<Vec<String>> {
        if components.is_empty() {
            return read_directory_names(self.root.raw());
        }
        let opened = self.open_path(components, FinalKind::Directory)?;
        read_directory_names(opened.raw())
    }

    fn open_path(&self, components: &[String], final_kind: FinalKind) -> Result<OwnedHandle> {
        let mut parent: Option<OwnedHandle> = None;
        for (index, component) in components.iter().enumerate() {
            let root = parent.as_ref().map_or(self.root.raw(), OwnedHandle::raw);
            let final_component = index + 1 == components.len();
            let kind = if final_component {
                final_kind
            } else {
                FinalKind::Directory
            };
            let opened = nt_open_relative(root, component, kind)?;
            let attributes = file_attributes(opened.raw())?;
            if attributes & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
                return Err(WboxError::spawn(format!(
                    "broker 拒绝 reparse path component '{}'",
                    component
                )));
            }
            if final_component {
                match final_kind {
                    FinalKind::File if attributes & FILE_ATTRIBUTE_DIRECTORY != 0 => {
                        return Err(WboxError::spawn("broker OPEN 只接受普通文件"));
                    }
                    FinalKind::Directory if attributes & FILE_ATTRIBUTE_DIRECTORY == 0 => {
                        return Err(WboxError::spawn("broker READDIR 只接受目录"));
                    }
                    _ => {}
                }
                return Ok(opened);
            }
            if attributes & FILE_ATTRIBUTE_DIRECTORY == 0 {
                return Err(WboxError::spawn(format!(
                    "broker path component '{}' 不是目录",
                    component
                )));
            }
            parent = Some(opened);
        }
        Err(WboxError::spawn("broker path 缺少路径组件"))
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
        let path =
            PathRequest::decode_payload(mount_id, &request.payload[OPEN_FIXED_LEN..], false)?;
        Ok(Self {
            mount_id,
            linux_flags,
            mode,
            components: path.components,
        })
    }
}

impl PathRequest {
    fn decode(
        request: &Request,
        opcode: u16,
        allow_empty: bool,
    ) -> std::result::Result<Self, &'static str> {
        if request.opcode != opcode || request.flags != 0 || request.payload.len() < 4 {
            return Err("opcode/flags/payload");
        }
        let mount_id = u32::from_le_bytes(request.payload[0..4].try_into().unwrap());
        Self::decode_payload(mount_id, &request.payload[4..], allow_empty)
    }

    fn decode_payload(
        mount_id: u32,
        path_bytes: &[u8],
        allow_empty: bool,
    ) -> std::result::Result<Self, &'static str> {
        if mount_id == 0 {
            return Err("mount id");
        }
        if path_bytes.len() > MAX_RELATIVE_PATH {
            return Err("path too long");
        }
        let path = std::str::from_utf8(path_bytes).map_err(|_| "path utf8")?;
        if path.starts_with('/') || path.contains('\\') || path.contains(':') || path.contains('\0')
        {
            return Err("path syntax");
        }
        let mut components = Vec::new();
        if !path.is_empty() {
            for component in path.split('/') {
                if component.is_empty() || component == "." || component == ".." {
                    return Err("path component");
                }
                components.push(component.to_string());
            }
        } else if !allow_empty {
            return Err("missing path");
        }
        Ok(Self {
            mount_id,
            components,
        })
    }
}

impl ReadDirRequest {
    fn decode(request: &Request) -> std::result::Result<Self, &'static str> {
        if request.opcode != OP_READDIR
            || request.flags != 0
            || request.payload.len() < READDIR_FIXED_LEN
        {
            return Err("opcode/flags/payload");
        }
        let mount_id = u32::from_le_bytes(request.payload[0..4].try_into().unwrap());
        let cursor = u32::from_le_bytes(request.payload[4..8].try_into().unwrap());
        Ok(Self {
            path: PathRequest::decode_payload(
                mount_id,
                &request.payload[READDIR_FIXED_LEN..],
                true,
            )?,
            cursor,
        })
    }
}

fn file_attributes(handle: HANDLE) -> Result<u32> {
    let mut info: FILE_ATTRIBUTE_TAG_INFO = unsafe { std::mem::zeroed() };
    let ok = unsafe {
        GetFileInformationByHandleEx(
            handle,
            FileAttributeTagInfo,
            (&mut info as *mut FILE_ATTRIBUTE_TAG_INFO).cast(),
            std::mem::size_of::<FILE_ATTRIBUTE_TAG_INFO>() as u32,
        )
    };
    if ok == 0 {
        return Err(last_error(
            "GetFileInformationByHandleEx(FileAttributeTagInfo)",
        ));
    }
    Ok(info.FileAttributes)
}

fn metadata_from_handle(handle: HANDLE) -> Result<BrokerMetadata> {
    let attributes = file_attributes(handle)?;
    let mut info: BY_HANDLE_FILE_INFORMATION = unsafe { std::mem::zeroed() };
    if unsafe { GetFileInformationByHandle(handle, &mut info) } == 0 {
        return Err(last_error("GetFileInformationByHandle(broker metadata)"));
    }
    let size = (u64::from(info.nFileSizeHigh) << 32) | u64::from(info.nFileSizeLow);
    let file_index = (u64::from(info.nFileIndexHigh) << 32) | u64::from(info.nFileIndexLow);
    let inode = (u64::from(info.dwVolumeSerialNumber) << 32) ^ file_index;
    let mode = if attributes & FILE_ATTRIBUTE_DIRECTORY != 0 {
        0o040_555
    } else {
        0o100_444
    };
    Ok(BrokerMetadata { mode, size, inode })
}

fn read_directory_names(handle: HANDLE) -> Result<Vec<String>> {
    let mut names = Vec::new();
    let mut name_bytes = 0usize;
    let mut restart = 1u8;
    loop {
        let mut storage = vec![0u64; 2048];
        let mut io = NtIoStatusBlock {
            status: 0,
            information: 0,
        };
        let status = unsafe {
            NtQueryDirectoryFile(
                handle,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                &mut io,
                storage.as_mut_ptr().cast(),
                (storage.len() * std::mem::size_of::<u64>()) as u32,
                FILE_NAMES_INFORMATION_CLASS,
                0,
                std::ptr::null_mut(),
                restart,
            )
        };
        restart = 0;
        if status == STATUS_NO_MORE_FILES_NT {
            break;
        }
        if status < 0 && status != STATUS_BUFFER_OVERFLOW_NT {
            return Err(WboxError::spawn(format!(
                "NtQueryDirectoryFile 失败，NTSTATUS=0x{:08X}",
                status as u32
            )));
        }
        let used = io
            .information
            .min(storage.len() * std::mem::size_of::<u64>());
        if used == 0 {
            if status == STATUS_BUFFER_OVERFLOW_NT {
                return Err(WboxError::spawn(
                    "NtQueryDirectoryFile 缓冲区不足且未返回条目",
                ));
            }
            break;
        }
        let bytes = unsafe { std::slice::from_raw_parts(storage.as_ptr().cast::<u8>(), used) };
        let mut offset = 0usize;
        loop {
            if offset.checked_add(12).is_none_or(|end| end > bytes.len()) {
                return Err(WboxError::spawn("NtQueryDirectoryFile 返回截断的目录项"));
            }
            let next = u32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap()) as usize;
            let name_len =
                u32::from_le_bytes(bytes[offset + 8..offset + 12].try_into().unwrap()) as usize;
            let name_end = offset
                .checked_add(12)
                .and_then(|start| start.checked_add(name_len))
                .ok_or_else(|| WboxError::spawn("NtQueryDirectoryFile 目录项长度溢出"))?;
            if !name_len.is_multiple_of(2) || name_end > bytes.len() {
                return Err(WboxError::spawn("NtQueryDirectoryFile 返回无效文件名长度"));
            }
            let utf16 = unsafe {
                std::slice::from_raw_parts(
                    bytes[offset + 12..name_end].as_ptr().cast::<u16>(),
                    name_len / 2,
                )
            };
            let name = String::from_utf16(utf16)
                .map_err(|_| WboxError::spawn("NtQueryDirectoryFile 返回非法 UTF-16 文件名"))?;
            if name != "." && name != ".." {
                name_bytes = name_bytes
                    .checked_add(name.len())
                    .ok_or_else(|| WboxError::spawn("broker 目录名总长度溢出"))?;
                if names.len() >= MAX_DIRECTORY_ENTRIES || name_bytes > MAX_DIRECTORY_NAME_BYTES {
                    return Err(WboxError::spawn("broker 目录超过枚举硬上限"));
                }
                names.push(name);
            }
            if next == 0 {
                break;
            }
            offset = offset
                .checked_add(next)
                .ok_or_else(|| WboxError::spawn("NtQueryDirectoryFile next offset 溢出"))?;
            if offset >= bytes.len() {
                return Err(WboxError::spawn("NtQueryDirectoryFile next offset 越界"));
            }
        }
    }
    names.sort();
    names.dedup();
    Ok(names)
}

fn nt_open_relative(root: HANDLE, name: &str, kind: FinalKind) -> Result<OwnedHandle> {
    let mut name_wide: Vec<u16> = name.encode_utf16().collect();
    let byte_len = name_wide
        .len()
        .checked_mul(2)
        .and_then(|length| u16::try_from(length).ok())
        .ok_or_else(|| WboxError::spawn("broker path component 过长"))?;
    let mut unicode = NtUnicodeString {
        length: byte_len,
        maximum_length: byte_len,
        buffer: name_wide.as_mut_ptr(),
    };
    let mut attributes = NtObjectAttributes {
        length: std::mem::size_of::<NtObjectAttributes>() as u32,
        root_directory: root,
        object_name: &mut unicode,
        attributes: OBJ_CASE_INSENSITIVE,
        security_descriptor: std::ptr::null_mut(),
        security_quality_of_service: std::ptr::null_mut(),
    };
    let mut io = NtIoStatusBlock {
        status: 0,
        information: 0,
    };
    let mut opened = std::ptr::null_mut();
    let desired_access =
        FILE_READ_DATA | FILE_LIST_DIRECTORY | FILE_READ_ATTRIBUTES | SYNCHRONIZE_ACCESS;
    let create_options = FILE_SYNCHRONOUS_IO_NONALERT
        | FILE_OPEN_REPARSE_POINT_OPTION
        | match kind {
            FinalKind::File => FILE_NON_DIRECTORY_FILE,
            FinalKind::Directory => FILE_DIRECTORY_FILE,
            FinalKind::Any => 0,
        };
    let status = unsafe {
        NtCreateFile(
            &mut opened,
            desired_access,
            &mut attributes,
            &mut io,
            std::ptr::null_mut(),
            0,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            FILE_OPEN,
            create_options,
            std::ptr::null_mut(),
            0,
        )
    };
    if status < 0 {
        return Err(WboxError::spawn(format!(
            "NtCreateFile(relative '{}') 失败，NTSTATUS=0x{:08X}",
            name, status as u32
        )));
    }
    Ok(OwnedHandle(opened))
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
            mounts,
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
            mounts: self.mounts,
        })
    }
}

pub(crate) struct BrokerSession {
    pipe: OwnedHandle,
    process: OwnedHandle,
    pid: u32,
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

    fn serve(self, expect_open: bool) -> Result<()> {
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
        if ping_status == STATUS_OK && expect_open {
            self.serve_open_loop()?;
        }
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

    fn serve_open_loop(&self) -> Result<()> {
        while let Some(request) = read_request_or_disconnect(self.pipe.raw())? {
            match request.opcode {
                OP_OPEN => self.serve_open(request)?,
                OP_LOOKUP => self.serve_lookup(request)?,
                OP_READDIR => self.serve_readdir(request)?,
                _ => self.write_status(&request, STATUS_PROTOCOL, Vec::new())?,
            }
        }
        Ok(())
    }

    fn write_status(&self, request: &Request, status: i32, payload: Vec<u8>) -> Result<()> {
        write_response(
            self.pipe.raw(),
            &Response {
                opcode: request.opcode,
                request_id: request.request_id,
                status,
                payload,
            },
        )
    }

    fn mount(&self, id: u32) -> Option<&BrokerMount> {
        self.mounts.iter().find(|mount| mount.id == id)
    }

    fn serve_lookup(&self, request: Request) -> Result<()> {
        let parsed = match PathRequest::decode(&request, OP_LOOKUP, true) {
            Ok(parsed) => parsed,
            Err(_) => return self.write_status(&request, STATUS_PROTOCOL, Vec::new()),
        };
        let Some(mount) = self.mount(parsed.mount_id) else {
            return self.write_status(&request, STATUS_NOT_FOUND, Vec::new());
        };
        let metadata = match mount.lookup(&parsed.components) {
            Ok(metadata) => metadata,
            Err(_) => return self.write_status(&request, STATUS_NOT_FOUND, Vec::new()),
        };
        let mut payload = Vec::with_capacity(24);
        payload.extend_from_slice(&metadata.mode.to_le_bytes());
        payload.extend_from_slice(&0u32.to_le_bytes());
        payload.extend_from_slice(&metadata.size.to_le_bytes());
        payload.extend_from_slice(&metadata.inode.to_le_bytes());
        self.write_status(&request, STATUS_OK, payload)
    }

    fn serve_readdir(&self, request: Request) -> Result<()> {
        let parsed = match ReadDirRequest::decode(&request) {
            Ok(parsed) => parsed,
            Err(_) => return self.write_status(&request, STATUS_PROTOCOL, Vec::new()),
        };
        let Some(mount) = self.mount(parsed.path.mount_id) else {
            return self.write_status(&request, STATUS_NOT_FOUND, Vec::new());
        };
        let names = match mount.read_directory(&parsed.path.components) {
            Ok(names) => names,
            Err(_) => return self.write_status(&request, STATUS_AUTH, Vec::new()),
        };
        let cursor = parsed.cursor as usize;
        if cursor > names.len() {
            return self.write_status(&request, STATUS_PROTOCOL, Vec::new());
        }
        let payload = encode_readdir_page(&names, cursor)?;
        self.write_status(&request, STATUS_OK, payload)
    }

    fn serve_open(&self, request: Request) -> Result<()> {
        let request_id = request.request_id;
        let opcode = request.opcode;
        let parsed = match OpenRequest::decode(&request) {
            Ok(parsed) => parsed,
            Err(_) => {
                return write_response(
                    self.pipe.raw(),
                    &Response {
                        opcode,
                        request_id,
                        status: STATUS_PROTOCOL,
                        payload: Vec::new(),
                    },
                );
            }
        };
        let Some(mount) = self.mount(parsed.mount_id) else {
            return write_response(
                self.pipe.raw(),
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
                    self.pipe.raw(),
                    &Response {
                        opcode,
                        request_id,
                        status: STATUS_AUTH,
                        payload: Vec::new(),
                    },
                );
            }
        };
        let metadata = metadata_from_handle(opened.raw())?;
        let mut remote = std::ptr::null_mut();
        if unsafe {
            DuplicateHandle(
                GetCurrentProcess(),
                opened.raw(),
                self.process.raw(),
                &mut remote,
                0,
                0,
                DUPLICATE_SAME_ACCESS,
            )
        } == 0
        {
            return Err(last_error("DuplicateHandle(broker OPEN)"));
        }
        let mut payload = Vec::with_capacity(32);
        payload.extend_from_slice(&(remote as usize as u64).to_le_bytes());
        payload.extend_from_slice(&metadata.mode.to_le_bytes());
        payload.extend_from_slice(&0u32.to_le_bytes());
        payload.extend_from_slice(&metadata.size.to_le_bytes());
        payload.extend_from_slice(&metadata.inode.to_le_bytes());
        let response = Response {
            opcode,
            request_id,
            status: STATUS_OK,
            payload,
        };
        if let Err(write_error) = write_response(self.pipe.raw(), &response) {
            let mut reclaimed = std::ptr::null_mut();
            let cleanup = unsafe {
                DuplicateHandle(
                    self.process.raw(),
                    remote,
                    GetCurrentProcess(),
                    &mut reclaimed,
                    0,
                    0,
                    DUPLICATE_CLOSE_SOURCE | DUPLICATE_SAME_ACCESS,
                )
            };
            if cleanup == 0 {
                return Err(WboxError::spawn(format!(
                    "{}；撤销目标进程 broker OPEN HANDLE 也失败：GetLastError={}",
                    write_error,
                    unsafe { GetLastError() }
                )));
            }
            drop(OwnedHandle(reclaimed));
            return Err(write_error);
        }
        Ok(())
    }
}

fn encode_readdir_page(names: &[String], cursor: usize) -> Result<Vec<u8>> {
    if cursor > names.len() {
        return Err(WboxError::spawn("broker READDIR cursor 越界"));
    }
    let mut payload = vec![0u8; 12];
    let mut next = cursor;
    while next < names.len() {
        let bytes = names[next].as_bytes();
        let name_len = u16::try_from(bytes.len())
            .map_err(|_| WboxError::spawn("broker READDIR 文件名过长"))?;
        if payload.len() + 2 + bytes.len() > MAX_PAYLOAD {
            break;
        }
        payload.extend_from_slice(&name_len.to_le_bytes());
        payload.extend_from_slice(bytes);
        next += 1;
    }
    if next == cursor && cursor < names.len() {
        return Err(WboxError::spawn("broker READDIR 单个文件名超过响应上限"));
    }
    payload[0..4].copy_from_slice(&(next as u32).to_le_bytes());
    payload[4..8].copy_from_slice(&u32::from(next == names.len()).to_le_bytes());
    payload[8..12].copy_from_slice(&((next - cursor) as u32).to_le_bytes());
    Ok(payload)
}

fn read_request(pipe: HANDLE) -> Result<Request> {
    read_request_or_disconnect(pipe)?.ok_or_else(|| WboxError::spawn("broker 请求提前 EOF"))
}

fn read_request_or_disconnect(pipe: HANDLE) -> Result<Option<Request>> {
    let mut header = [0u8; REQUEST_HEADER_LEN];
    let mut read = 0u32;
    let ok = unsafe {
        ReadFile(
            pipe,
            header.as_mut_ptr(),
            header.len() as u32,
            &mut read,
            std::ptr::null_mut(),
        )
    };
    if ok == 0 {
        let error = unsafe { GetLastError() };
        if error == ERROR_BROKEN_PIPE || error == ERROR_NO_DATA {
            return Ok(None);
        }
        return Err(WboxError::spawn(format!(
            "ReadFile(broker request header) 失败，GetLastError={}",
            error
        )));
    }
    if read == 0 {
        return Ok(None);
    }
    if read as usize != header.len() {
        return Err(WboxError::spawn(format!(
            "broker 请求 header 截断：{} != {}",
            read,
            header.len()
        )));
    }
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
        .map(Some)
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

    fn path_request(opcode: u16, path: &[u8], mount_id: u32, request_id: u64) -> Request {
        let mut payload = Vec::with_capacity(4 + path.len());
        payload.extend_from_slice(&mount_id.to_le_bytes());
        payload.extend_from_slice(path);
        Request {
            opcode,
            request_id,
            flags: 0,
            payload,
        }
    }

    fn readdir_request(path: &[u8], mount_id: u32, cursor: u32, request_id: u64) -> Request {
        let mut payload = Vec::with_capacity(READDIR_FIXED_LEN + path.len());
        payload.extend_from_slice(&mount_id.to_le_bytes());
        payload.extend_from_slice(&cursor.to_le_bytes());
        payload.extend_from_slice(path);
        Request {
            opcode: OP_READDIR,
            request_id,
            flags: 0,
            payload,
        }
    }

    fn decode_readdir_names(payload: &[u8]) -> Vec<String> {
        assert!(payload.len() >= 12);
        let count = u32::from_le_bytes(payload[8..12].try_into().unwrap()) as usize;
        let mut names = Vec::with_capacity(count);
        let mut offset = 12;
        for _ in 0..count {
            let len = u16::from_le_bytes(payload[offset..offset + 2].try_into().unwrap()) as usize;
            offset += 2;
            names.push(
                std::str::from_utf8(&payload[offset..offset + len])
                    .unwrap()
                    .to_string(),
            );
            offset += len;
        }
        assert_eq!(offset, payload.len());
        names
    }

    #[test]
    fn readdir_pages_stay_bounded_and_resume_by_cursor() {
        let names = (0..500)
            .map(|index| format!("entry-{index:04}-{}", "x".repeat(24)))
            .collect::<Vec<_>>();
        let first = encode_readdir_page(&names, 0).unwrap();
        assert!(first.len() <= MAX_PAYLOAD);
        let first_cursor = u32::from_le_bytes(first[0..4].try_into().unwrap()) as usize;
        assert!(first_cursor > 0 && first_cursor < names.len());
        assert_eq!(u32::from_le_bytes(first[4..8].try_into().unwrap()), 0);
        assert_eq!(decode_readdir_names(&first), names[..first_cursor]);

        let second = encode_readdir_page(&names, first_cursor).unwrap();
        assert!(second.len() <= MAX_PAYLOAD);
        assert_eq!(
            decode_readdir_names(&second),
            names[first_cursor..u32::from_le_bytes(second[0..4].try_into().unwrap()) as usize]
        );
        assert!(encode_readdir_page(&names, names.len() + 1).is_err());
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
        std::fs::remove_dir_all(root).unwrap();
        std::fs::remove_dir_all(outside).unwrap();
    }

    #[test]
    fn readonly_mount_looks_up_and_enumerates_without_host_paths() {
        let root = std::env::temp_dir().join(format!(
            "wbox_broker_lookup_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(root.join("nested")).unwrap();
        std::fs::write(root.join("nested").join("canary.txt"), b"canary").unwrap();
        std::fs::write(root.join("root.txt"), b"root").unwrap();

        let mount = BrokerMount::open_readonly(1, &root).unwrap();
        let root_metadata = mount.lookup(&[]).unwrap();
        assert_eq!(root_metadata.mode & 0o170_000, 0o040_000);
        let file_metadata = mount
            .lookup(&["nested".to_string(), "canary.txt".to_string()])
            .unwrap();
        assert_eq!(file_metadata.mode & 0o170_000, 0o100_000);
        assert_eq!(file_metadata.size, 6);
        assert_eq!(
            mount.read_directory(&[]).unwrap(),
            ["nested".to_string(), "root.txt".to_string()]
        );
        assert_eq!(
            mount.read_directory(&["nested".to_string()]).unwrap(),
            ["canary.txt".to_string()]
        );

        drop(mount);
        std::fs::remove_dir_all(root).unwrap();
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

        if let Ok(path) = std::env::var("WBOX_TEST_BROKER_OPEN_PATH") {
            for request_id in [3, 4] {
                let mut request = open_request(path.as_bytes(), 1, 0, 0);
                request.request_id = request_id;
                write_request(pipe.raw(), &request).unwrap();
                let opened = read_response(pipe.raw()).unwrap();
                assert_eq!(opened.status, STATUS_OK, "broker OPEN failed");
                assert_eq!(opened.request_id, request_id);
                assert_eq!(opened.payload.len(), 32);
                let remote =
                    u64::from_le_bytes(opened.payload[0..8].try_into().unwrap()) as usize as HANDLE;
                assert_eq!(
                    u32::from_le_bytes(opened.payload[8..12].try_into().unwrap()) & 0o170_000,
                    0o100_000
                );
                let remote = OwnedHandle(remote);
                let mut buffer = [0u8; 64];
                let mut read = 0;
                assert_ne!(
                    unsafe {
                        ReadFile(
                            remote.raw(),
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

            write_request(pipe.raw(), &path_request(OP_LOOKUP, b"", 1, 5)).unwrap();
            let root = read_response(pipe.raw()).unwrap();
            assert_eq!(root.status, STATUS_OK);
            assert_eq!(root.payload.len(), 24);
            assert_eq!(
                u32::from_le_bytes(root.payload[0..4].try_into().unwrap()) & 0o170_000,
                0o040_000
            );

            write_request(pipe.raw(), &path_request(OP_LOOKUP, path.as_bytes(), 1, 6)).unwrap();
            let file = read_response(pipe.raw()).unwrap();
            assert_eq!(file.status, STATUS_OK);
            assert_eq!(
                u32::from_le_bytes(file.payload[0..4].try_into().unwrap()) & 0o170_000,
                0o100_000
            );
            assert_eq!(
                u64::from_le_bytes(file.payload[8..16].try_into().unwrap()),
                std::env::var("WBOX_TEST_BROKER_OPEN_EXPECTED")
                    .unwrap()
                    .len() as u64
            );

            write_request(pipe.raw(), &readdir_request(b"", 1, 0, 7)).unwrap();
            let root_entries = read_response(pipe.raw()).unwrap();
            assert_eq!(root_entries.status, STATUS_OK);
            assert!(decode_readdir_names(&root_entries.payload).contains(&"nested".to_string()));

            write_request(pipe.raw(), &readdir_request(b"nested", 1, 0, 8)).unwrap();
            let nested_entries = read_response(pipe.raw()).unwrap();
            assert_eq!(nested_entries.status, STATUS_OK);
            assert_eq!(
                decode_readdir_names(&nested_entries.payload),
                ["canary.txt".to_string()]
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
