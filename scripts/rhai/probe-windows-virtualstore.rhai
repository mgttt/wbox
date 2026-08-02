// Windows VirtualStore probe. Args: [WBOX executable]
fn require_ok(output, code) {
    if !output.success { throw code + "\n" + output.stdout_text() + output.stderr_text(); }
}
fn run(program, argv, cwd, timeout_seconds, code) {
    let command = std::process::command(program);
    command.args(argv); command.current_dir(cwd);
    command.timeout(std::time::Duration::from_secs(timeout_seconds));
    command.capture_limit(262144);
    let output = command.output(); require_ok(output, code); output
}
let repo = std::env::current_dir().display;
let wbox = if args.len > 0 { std::path::absolute(args[0]).display }
    else { std::path::join(repo, "target/release/wbox.exe").display };
let sandbox = std::path::join(rhai::runtime::temp_dir().display,
    "wbox-virtualstore-" + std::process::id()).display;
let probe = std::path::join(repo, "dev/windows-virtualstore-probe.rs").display;
let probe_exe = std::path::join(sandbox, "windows-virtualstore-probe.exe").display;
let file_name = "wbox-virtualstore-probe-" + std::process::id() + ".txt";
let program_files = std::env::get("ProgramFiles");
let local_appdata = std::env::get("LOCALAPPDATA");
let real_path = std::path::join(program_files, file_name).display;
let virtual_path = std::path::join(local_appdata, "VirtualStore/Program Files/" + file_name).display;
let container = "w3a-" + std::process::id();
fn cleanup(wbox, container, real_path, virtual_path, sandbox) {
    let command = std::process::command(wbox); command.args(["rm", "-f", container]); command.timeout(std::time::Duration::from_secs(60)); command.output();
    if std::fs::exists(real_path) { std::fs::remove_file(real_path); }
    if std::fs::exists(virtual_path) { std::fs::remove_file(virtual_path); }
    if std::fs::exists(sandbox) { std::fs::remove_dir_all(sandbox); }
}
try {
    std::fs::create_dir_all(sandbox);
    require_ok(run("rustc", ["--target", "i686-pc-windows-msvc", "-O", probe, "-o", probe_exe], repo, 300, "probe_build"), "probe_build");
    require_ok(run("icacls.exe", [sandbox, "/grant", "*S-1-15-2-1:(OI)(CI)(RX)", "/T", "/C"], repo, 60, "probe_acl"), "probe_acl");
    if std::fs::exists(real_path) { std::fs::remove_file(real_path); }
    if std::fs::exists(virtual_path) { std::fs::remove_file(virtual_path); }
    let result = run(wbox, ["run", "--name", container, "--workdir", std::env::get("SystemRoot") + "\\System32", "--", probe_exe, real_path], repo, 120, "probe_wbox");
    if !result.stdout_text().contains("POINTER_WIDTH=32 MANIFEST_PRESENT=false") {
        throw "probe_preconditions:\n" + result.stdout_text();
    }
    print(rhai::json::stringify_pretty(#{ probe_exit_code: result.exit_code,
        real_path: real_path, real_path_exists: std::fs::exists(real_path),
        virtual_path: virtual_path, virtual_path_exists: std::fs::exists(virtual_path),
        output: result.stdout_text() }));
    cleanup(wbox, container, real_path, virtual_path, sandbox);
} catch (error) {
    cleanup(wbox, container, real_path, virtual_path, sandbox);
    throw error;
}
