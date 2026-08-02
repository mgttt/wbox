// WP.1-WP.5 product core: native Windows path, Linux fixture path, and state cleanup.
// Args: WBOX WBOX_LINUX BUSYBOX.
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
        if entry.is_dir { copy_tree(entry.path.display, target); } else { std::fs::copy(entry.path.display, target); }
    }
}
fn check(output, expected, label, marker) {
    let text = output.combined_text();
    require(output.exit_code == expected, label + " expected rc=" + expected.to_string() + " got=" + output.exit_code.to_string() + "\n" + text);
    if marker != "" { require(text.contains(marker), label + " missing marker:" + marker); }
    print("PASS " + label);
}
let repo = std::env::current_dir().display;
require(args.len >= 3, "expected WBOX WBOX_LINUX BUSYBOX");
let wbox = std::path::absolute(args[0]).display;
let linux = std::path::absolute(args[1]).display;
let busybox = std::path::absolute(args[2]).display;
for path in [wbox, linux, busybox] { require(std::fs::metadata(path).is_file, "missing_product_file:" + path); }
let tag = std::process::id().to_string();
let sandbox = std::path::join(rhai::runtime::temp_dir().display, "wbox-product-" + tag).display;
let bundle = std::path::join(sandbox, "bundle").display;
let home = std::path::join(sandbox, "home").display;
let image = std::path::join(home, ".wbox/images/local.test/wbox-fixture/latest").display;
let rootfs = std::path::join(image, "rootfs").display;
let system_root = std::env::get("SystemRoot");
let system32 = std::path::join(system_root, "System32").display;
let env = #{USERPROFILE: home, HOME: home};
try {
    std::fs::create_dir_all(std::path::join(rootfs, "probe").display);
    std::fs::create_dir_all(std::path::join(rootfs, "bin").display);
    std::fs::copy(busybox, std::path::join(rootfs, "busybox").display);
    std::fs::copy(busybox, std::path::join(rootfs, "bin/sh").display);
    std::fs::write(std::path::join(rootfs, "probe/directory-entry-ok").display, "ok");
    std::fs::write(std::path::join(image, "manifest.json").display, "{}");
    std::fs::write(std::path::join(image, "layers.json").display, "[]");
    std::fs::write(std::path::join(image, "config.json").display, `{"config":{"Entrypoint":[],"Cmd":["/busybox","echo","PRODUCT_E2E_OK"],"Env":["PATH=/"],"WorkingDir":"/"}}`);
    let acl = std::process::command("icacls.exe"); acl.args([rootfs, "/grant", "*S-1-15-2-1:(OI)(CI)(RX)", "/T", "/C"]); acl.timeout(std::time::Duration::from_secs(60)); require(acl.output().success, "rootfs_acl_failed");
    std::fs::create_dir_all(bundle); std::fs::copy(wbox, std::path::join(bundle, "wbox.exe").display); std::fs::copy(linux, std::path::join(bundle, "wbox-linux.exe").display);
    let portable = std::path::join(bundle, "wbox.exe").display;
    check(run(portable, ["run", "--name", "product-native", "--workdir", system32, "--", "cmd.exe", "/d", "/c", "echo WINDOWS_NATIVE_E2E_OK"], repo, 120, env), 0, "WP.1 Windows native product path", "WINDOWS_NATIVE_E2E_OK");
    let filtered_env = #{USERPROFILE: home, HOME: home, HOST_ONLY_MARKER: "must-not-leak"};
    check(run(portable, ["run", "--name", "product-env", "--workdir", system32, "--", "cmd.exe", "/d", "/c", "if defined HOST_ONLY_MARKER (exit /b 9) else (echo ENV_FILTER_E2E_OK)"], repo, 120, filtered_env), 0, "WP.2 Windows environment filter product path", "ENV_FILTER_E2E_OK");
    let guest = run(portable, ["run", "--name", "product-guest", "local.test/wbox-fixture:latest"], repo, 120, env); check(guest, 0, "WP.3 Windows OCI-to-Linux product path", "PRODUCT_E2E_OK"); print("PASS WP.4 portable two-executable bundle");
    let linux_env = run(portable, ["run", "--name", "product-linux-env", "-e", "FOO=upper", "-e", "foo=lower", "local.test/wbox-fixture:latest", "/busybox", "env"], repo, 120, env); check(linux_env, 0, "WP.3E environment boundary", "FOO=upper"); require(linux_env.stdout_text().contains("foo=lower"), "WP.3E case-sensitive entry missing");
    let directory = run(portable, ["run", "--name", "product-dir", "local.test/wbox-fixture:latest", "/busybox", "ls", "/probe"], repo, 120, env); check(directory, 0, "WP.3D directory enumeration", "directory-entry-ok");
    let write = run(portable, ["run", "--name", "product-write", "local.test/wbox-fixture:latest", "/busybox", "sh", "-c", "echo PRIVATE_WRITE_OK > /probe/private-write && /busybox cat /probe/private-write"], repo, 120, env); check(write, 0, "WP.3W private writable rootfs", "PRIVATE_WRITE_OK"); require(!std::fs::exists(std::path::join(rootfs, "probe/private-write").display), "WP.3W shared cache mutated");
    let help = run(std::path::join(bundle, "wbox-linux.exe").display, ["--help"], rootfs, 60, #{}); check(help, 0, "WP.4 wbox-linux identity help", "internal Linux ELF runtime"); require(help.stdout_text().contains("Use wbox.exe"), "WP.4 help missing owner hint");
    let trace = run(std::path::join(bundle, "wbox-linux.exe").display, ["-s", "-e", "/busybox", "true"], rootfs, 60, #{BLINK_PREFIX: rootfs}); check(trace, 0, "WP.4S release syscall trace", "(sys)");
    let state = run(portable, ["ps", "--all"], repo, 60, env); check(state, 0, "WP.5 normal-run state inspection", ""); require(!state.stdout_text().contains("product-native") && !state.stdout_text().contains("product-guest"), "WP.5 foreground state leaked"); print("PASS WP.5 normal-run state cleanup");
} catch (error) {
    let cleanup = std::process::command(wbox); cleanup.args(["rm", "-f", "product-native", "product-env", "product-guest", "product-linux-env", "product-dir", "product-write"]); cleanup.timeout(std::time::Duration::from_secs(60)); cleanup.output(); if std::fs::exists(sandbox) { std::fs::remove_dir_all(sandbox); } throw error;
}
let cleanup = std::process::command(wbox); cleanup.args(["rm", "-f", "product-native", "product-env", "product-guest", "product-linux-env", "product-dir", "product-write"]); cleanup.timeout(std::time::Duration::from_secs(60)); cleanup.output(); if std::fs::exists(sandbox) { std::fs::remove_dir_all(sandbox); }
