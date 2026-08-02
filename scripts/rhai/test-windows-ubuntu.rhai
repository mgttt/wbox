// Pinned Ubuntu fixture product journey. Args: WBOX WBOX_LINUX UBUNTU_IMAGE.
fn require(condition, message) { if !condition { throw message; } }
fn run(program, argv, cwd, timeout, env) {
    let command = std::process::command(program); command.args(argv); command.current_dir(cwd);
    for key in env.keys() { command.env(key, env[key]); }
    command.timeout(std::time::Duration::from_secs(timeout)); command.capture_limit(262144); command.output()
}
fn copy_tree(source, destination) {
    std::fs::create_dir_all(destination);
    for entry in std::fs::read_dir(source) {
        let target = std::path::join(destination, entry.file_name).display;
        if entry.is_dir { copy_tree(entry.path.display, target); }
        else { std::fs::copy(entry.path.display, target); }
    }
}
let repo = std::env::current_dir().display;
require(args.len >= 3, "expected WBOX WBOX_LINUX UBUNTU_IMAGE");
let wbox = std::path::absolute(args[0]).display;
let linux = std::path::absolute(args[1]).display;
let source = std::path::absolute(args[2]).display;
require(std::fs::metadata(wbox).is_file && std::fs::metadata(linux).is_file, "product_binaries_missing");
for required in ["rootfs", "manifest.json", "config.json", "layers.json"] {
    require(std::fs::exists(std::path::join(source, required).display), "ubuntu_fixture_missing:" + required);
}
let expected = "52df9b1ee71626e0088f7d400d5c6b5f7bb916f8f0c82b474289a4ece6cf3faf";
let manifest = std::path::join(source, "manifest.json").display;
let hash = rhai::crypto::sha256(std::fs::read(manifest)).to_lower();
require(hash == expected, "ubuntu_fixture_manifest_hash_mismatch:" + hash);
let scratch = std::path::join(rhai::runtime::temp_dir().display, "wbox-ubuntu-" + std::process::id()).display;
let bundle = std::path::join(scratch, "bundle").display;
let home = std::path::join(scratch, "home").display;
let image = std::path::join(home, ".wbox/images/local.test/ubuntu-24.04/latest").display;
let probe = `. /etc/os-release
printf 'OS=%s:%s\n' "$ID" "$VERSION_ID"
/usr/bin/uname -m
/bin/bash --version
/usr/bin/apt --version
/usr/bin/dpkg --print-architecture
/usr/bin/getconf LONG_BIT
exit 37`;
std::fs::create_dir_all(bundle); std::fs::create_dir_all(image);
try {
    std::fs::copy(wbox, std::path::join(bundle, "wbox.exe").display);
    std::fs::copy(linux, std::path::join(bundle, "wbox-linux.exe").display);
    copy_tree(source, image);
    let acl = std::process::command("icacls.exe"); acl.args([std::path::join(image, "rootfs").display, "/grant", "*S-1-15-2-1:(OI)(CI)(RX)", "/T", "/C"]); acl.timeout(std::time::Duration::from_secs(60)); require(acl.output().success, "WU.1_acl_failed");
    print("PASS WU.1 pinned Ubuntu fixture and ACL");
    let env = #{USERPROFILE: home, HOME: home};
    let result = run(std::path::join(bundle, "wbox.exe").display, ["run", "--name", "product-ubuntu", "local.test/ubuntu-24.04:latest", "/bin/bash", "-c", probe], repo, 180, env);
    require(result.exit_code == 37, "WU.2 expected rc=37 got=" + result.exit_code.to_string());
    for marker in ["OS=ubuntu:24.04", "x86_64", "GNU bash, version 5.2", "apt 2.8.3 (amd64)", "amd64", "64"] {
        require(result.stdout_text().contains(marker), "WU.2 missing marker:" + marker);
    }
    print("PASS WU.2 Ubuntu product path");
} catch (error) {
    let cleanup = std::process::command(wbox); cleanup.args(["rm", "-f", "product-ubuntu"]); cleanup.timeout(std::time::Duration::from_secs(30)); cleanup.output();
    if std::fs::exists(scratch) { std::fs::remove_dir_all(scratch); }
    throw error;
}
let cleanup = std::process::command(wbox); cleanup.args(["rm", "-f", "product-ubuntu"]); cleanup.timeout(std::time::Duration::from_secs(30)); cleanup.output();
if std::fs::exists(scratch) { std::fs::remove_dir_all(scratch); }
