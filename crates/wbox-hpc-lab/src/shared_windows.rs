use std::io;

use windows_sys::Win32::Foundation::{CloseHandle, HANDLE, INVALID_HANDLE_VALUE};
use windows_sys::Win32::System::Memory::{
    CreateFileMappingW, MapViewOfFile, OpenFileMappingW, UnmapViewOfFile, FILE_MAP_ALL_ACCESS,
    MEMORY_MAPPED_VIEW_ADDRESS, PAGE_READWRITE,
};

pub struct SharedMapping {
    handle: HANDLE,
    view: MEMORY_MAPPED_VIEW_ADDRESS,
    name: String,
    len: usize,
}

impl SharedMapping {
    pub fn create(name: &str, len: usize) -> Result<Self, String> {
        if len == 0 {
            return Err("shared mapping cannot be empty".to_owned());
        }
        let wide = wide(name);
        let size = len as u64;
        // SAFETY: the name is terminated, the size is nonzero, and no security
        // attributes are retained. INVALID_HANDLE_VALUE selects page-file backing.
        let handle = unsafe {
            CreateFileMappingW(
                INVALID_HANDLE_VALUE,
                std::ptr::null(),
                PAGE_READWRITE,
                (size >> 32) as u32,
                size as u32,
                wide.as_ptr(),
            )
        };
        Self::map(handle, name, len, "CreateFileMappingW")
    }

    pub fn open(name: &str, len: usize) -> Result<Self, String> {
        let wide = wide(name);
        // SAFETY: the name is terminated and access is limited to the current
        // user's named object namespace by the caller's token.
        let handle = unsafe { OpenFileMappingW(FILE_MAP_ALL_ACCESS, 0, wide.as_ptr()) };
        Self::map(handle, name, len, "OpenFileMappingW")
    }

    fn map(handle: HANDLE, name: &str, len: usize, operation: &str) -> Result<Self, String> {
        if handle.is_null() {
            return Err(last_error(operation));
        }
        // SAFETY: handle is a valid file-mapping handle and len matches the
        // object size supplied by the parent.
        let view = unsafe { MapViewOfFile(handle, FILE_MAP_ALL_ACCESS, 0, 0, len) };
        if view.Value.is_null() {
            // SAFETY: handle was returned by a successful Win32 call.
            unsafe { CloseHandle(handle) };
            return Err(last_error("MapViewOfFile"));
        }
        Ok(Self {
            handle,
            view,
            name: name.to_owned(),
            len,
        })
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn as_ptr(&self) -> *const u8 {
        self.view.Value.cast()
    }

    pub fn as_mut_ptr(&self) -> *mut u8 {
        self.view.Value.cast()
    }

    #[allow(dead_code)]
    pub fn len(&self) -> usize {
        self.len
    }
}

impl Drop for SharedMapping {
    fn drop(&mut self) {
        // SAFETY: this value uniquely owns one mapped view and one handle.
        unsafe {
            UnmapViewOfFile(self.view);
            CloseHandle(self.handle);
        }
    }
}

fn wide(value: &str) -> Vec<u16> {
    value.encode_utf16().chain(std::iter::once(0)).collect()
}

fn last_error(operation: &str) -> String {
    format!("{operation}: {}", io::Error::last_os_error())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn named_mapping_round_trips_between_views() {
        let name = format!("Local\\wbox-hpc-map-test-{}", std::process::id());
        let first = SharedMapping::create(&name, 4096).unwrap();
        let second = SharedMapping::open(&name, 4096).unwrap();
        // SAFETY: both pointers address the same writable 4096-byte mapping.
        unsafe {
            first.as_mut_ptr().cast::<u64>().write(0x1234_5678);
            assert_eq!(second.as_ptr().cast::<u64>().read(), 0x1234_5678);
        }
    }
}
