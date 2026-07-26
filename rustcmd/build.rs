fn main() {
    println!("cargo:rerun-if-changed=assets/rustcmd.ico");
    #[cfg(windows)]
    winresource::WindowsResource::new()
        .set_icon("assets/rustcmd.ico")
        .compile()
        .expect("failed to embed RustCmd icon");
}
