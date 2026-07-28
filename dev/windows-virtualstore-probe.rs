use std::path::PathBuf;

#[link(name = "kernel32")]
extern "system" {
    fn FindResourceW(
        module: *mut core::ffi::c_void,
        name: *const u16,
        kind: *const u16,
    ) -> *mut core::ffi::c_void;
}

fn has_embedded_manifest() -> bool {
    const RT_MANIFEST: usize = 24;
    const CREATE_PROCESS_MANIFEST_RESOURCE_ID: usize = 1;
    // SAFETY: null selects the current executable. Integer resources are encoded
    // as pointer-sized values whose high word is zero.
    !unsafe {
        FindResourceW(
            std::ptr::null_mut(),
            CREATE_PROCESS_MANIFEST_RESOURCE_ID as *const u16,
            RT_MANIFEST as *const u16,
        )
    }
    .is_null()
}

fn main() {
    let manifest_present = has_embedded_manifest();
    println!(
        "POINTER_WIDTH={} MANIFEST_PRESENT={manifest_present}",
        usize::BITS
    );
    if usize::BITS != 32 || manifest_present {
        eprintln!("probe precondition failed");
        std::process::exit(39);
    }

    let Some(target) = std::env::args_os().nth(1).map(PathBuf::from) else {
        eprintln!("usage: windows-virtualstore-probe.exe TARGET");
        std::process::exit(2);
    };

    match std::fs::write(&target, b"WBOX_VIRTUALSTORE_PROBE") {
        Ok(()) => {
            println!("WRITE_OK path={}", target.display());
            match std::fs::read_to_string(&target) {
                Ok(value) => println!("READ_BACK={value}"),
                Err(error) => {
                    eprintln!("READ_BACK_ERROR kind={:?} error={error}", error.kind());
                    std::process::exit(38);
                }
            }
        }
        Err(error) => {
            eprintln!(
                "WRITE_ERROR kind={:?} os_error={:?} error={error}",
                error.kind(),
                error.raw_os_error()
            );
            std::process::exit(37);
        }
    }
}
