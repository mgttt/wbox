// WP.6–WP.7C detached lifecycle and inspect journeys. Args: WBOX BUSYBOX.
fn require(condition, message) { if !condition { throw message; } }
fn run(program, argv, cwd, timeout, env) {
    let command = std::process::command(program); command.args(argv); command.current_dir(cwd);
    for key in env.keys() { command.env(key, env[key]); }
    command.timeout(std::time::Duration::from_secs(timeout)); command.capture_limit(262144); command.output()
}
fn ok(output, label) { require(output.success, label + " failed:\n" + output.stdout_text() + output.stderr_text()); }
fn text(output) { output.combined_text() }
let repo = std::env::current_dir().display;
require(args.len >= 2, "expected WBOX BUSYBOX");
let wbox = std::path::absolute(args[0]).display;
let busybox = std::path::absolute(args[1]).display;
let tag = std::process::id().to_string();
let sandbox = std::path::join(rhai::runtime::temp_dir().display, "wbox-lifecycle-" + tag).display;
let home = std::path::join(sandbox, "home").display;
let image = std::path::join(home, ".wbox/images/local.test/wbox-fixture/latest").display;
let rootfs = std::path::join(image, "rootfs").display;
let system_root = std::env::get("SystemRoot");
let system32 = std::path::join(system_root, "System32").display;
let env = #{USERPROFILE: home, HOME: home};
try {
    std::fs::create_dir_all(std::path::join(rootfs, "probe").display);
    std::fs::copy(busybox, std::path::join(rootfs, "busybox").display);
    std::fs::write(std::path::join(image, "manifest.json").display, "{}");
    std::fs::write(std::path::join(image, "layers.json").display, "[]");
    std::fs::write(std::path::join(image, "config.json").display, `{"config":{"Entrypoint":[],"Cmd":["/busybox","echo","PRODUCT_E2E_OK"],"Env":["PATH=/"],"WorkingDir":"/"}}`);
    let acl = std::process::command("icacls.exe"); acl.args([rootfs, "/grant", "*S-1-15-2-1:(OI)(CI)(RX)", "/T", "/C"]); acl.timeout(std::time::Duration::from_secs(60)); require(acl.output().success, "lifecycle_acl_failed");
    let wbox_run = ["run", "-d", "--name", "product-bg", "--workdir", system32, "--", "cmd.exe", "/d", "/c", "echo DETACH_E2E_OK"];
    let bg = run(wbox, wbox_run, repo, 60, env); ok(bg, "WP.6 detach launch"); let bg_text = text(bg); bg_text.trim(); require(bg_text == "product-bg", "WP.6 detach output: <" + text(bg) + ">");
    let seen = false;
    for iteration in 0..30 { let state = run(wbox, ["ps", "--all"], repo, 30, env); if state.success && text(state).contains("product-bg") { seen = true; break; } rhai::task::sleep(std::time::Duration::from_millis(500)); }
    require(seen, "WP.6 detached state not observed");
    let logs = run(wbox, ["logs", "product-bg"], repo, 30, env); ok(logs, "WP.6 logs read"); require(text(logs).contains("DETACH_E2E_OK"), "WP.6 log marker"); print("PASS WP.6 detach -> ps -> logs");
    ok(run(wbox, ["rm", "product-bg"], repo, 30, env), "WP.7 rm detached record"); let after = run(wbox, ["ps", "--all"], repo, 30, env); ok(after, "WP.7 state inspection"); require(!text(after).contains("product-bg"), "WP.7 record survived rm"); print("PASS WP.7 rm clears detached record");
    let wait = run(wbox, ["run", "-d", "--name", "product-wait", "local.test/wbox-fixture:latest", "/busybox", "sh", "-c", "exit 37"], repo, 60, env); ok(wait, "WP.7B launch"); let waited = run(wbox, ["wait", "product-wait"], repo, 60, env); ok(waited, "WP.7B wait"); let waited_text = text(waited); waited_text.trim(); require(waited_text == "37", "WP.7B exit code");
    let inspect = run(wbox, ["container", "inspect", "product-wait"], repo, 30, env); ok(inspect, "WP.7C container inspect"); let container = rhai::json::parse(text(inspect))[0]; require(container.State.Status == "exited" && container.State.ExitCode == 37 && container.Config.Image == "local.test/wbox-fixture:latest", "WP.7C container JSON");
    let image_inspect = run(wbox, ["image", "inspect", "local.test/wbox-fixture:latest"], repo, 30, env); ok(image_inspect, "WP.7C image inspect"); let image_json = rhai::json::parse(text(image_inspect))[0]; require(image_json.Os == "linux" && image_json.Config.Cmd[0] == "/busybox", "WP.7C image JSON"); ok(run(wbox, ["rm", "product-wait"], repo, 30, env), "WP.7C rm waited record"); print("PASS WP.7B wait and WP.7C inspect");
    let auto_names = [];
    for index in 1..11 { let name = "product-auto-rm-" + index.to_string(); auto_names.push(name); ok(run(wbox, ["run", "-d", "--rm", "--name", name, "--workdir", system32, "--", "cmd.exe", "/d", "/c", "exit /b 0"], repo, 60, env), "WP.7A launch " + name); }
    for iteration in 0..30 {
        let remaining = false;
        for name in auto_names { if std::fs::exists(std::path::join(home, ".wbox/run/" + name).display) { remaining = true; } }
        if !remaining { break; }
        rhai::task::sleep(std::time::Duration::from_millis(200));
    }
    for name in auto_names { require(!std::fs::exists(std::path::join(home, ".wbox/run/" + name).display), "WP.7A state survived:" + name); }
    print("PASS WP.7A detached --rm cleanup");
} catch (error) {
    let cleanup = std::process::command(wbox); cleanup.args(["rm", "-f", "product-bg", "product-wait"]); cleanup.timeout(std::time::Duration::from_secs(30)); cleanup.output(); if std::fs::exists(sandbox) { std::fs::remove_dir_all(sandbox); } throw error;
}
let cleanup = std::process::command(wbox); cleanup.args(["rm", "-f", "product-bg", "product-wait"]); cleanup.timeout(std::time::Duration::from_secs(30)); cleanup.output(); if std::fs::exists(sandbox) { std::fs::remove_dir_all(sandbox); }
