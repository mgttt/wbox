// Windows network policy journey. Args: WBOX [ENDPOINT] [TIMEOUT_SECONDS]
fn require(condition, message) { if !condition { throw message; } }
fn run(program, argv, cwd, timeout) {
    let command = std::process::command(program); command.args(argv); command.current_dir(cwd);
    command.timeout(std::time::Duration::from_secs(timeout)); command.capture_limit(262144);
    command.output()
}
fn expect(output, code) { require(output.success, code + "\n" + output.stdout_text() + output.stderr_text()); }
let repo = std::env::current_dir().display;
require(args.len >= 1, "expected WBOX [ENDPOINT] [TIMEOUT_SECONDS]");
let wbox = std::path::absolute(args[0]).display;
let endpoint = if args.len > 1 { args[1] } else { "http://1.1.1.1/cdn-cgi/trace" };
let timeout = if args.len > 2 { args[2].parse_int() } else { 25 };
require(std::fs::exists(wbox) && std::fs::metadata(wbox).is_file, "wbox_not_file:" + wbox);
let system_root = std::env::get("SystemRoot");
let curl = std::path::join(system_root, "System32/curl.exe").display;
require(std::fs::exists(curl), "curl_not_found:" + curl);
let scratch = std::path::join(rhai::runtime::temp_dir().display,
    "wbox-network-" + std::process::id()).display;
let default_name = "wnet-deny-" + std::process::id();
let allowed_name = "wnet-allow-" + std::process::id();
std::fs::create_dir_all(scratch);
let curl_common = ["--silent", "--show-error", "--fail", "--connect-timeout", "8", "--max-time", "15"];
try {
    let host = run(curl, curl_common + ["--output", std::path::join(scratch, "host-response.txt").display, endpoint], repo, timeout);
    expect(host, "WNET.1 host endpoint reachable"); print("PASS WNET.1 host endpoint reachable");
    let denied = run(wbox, ["run", "--name", default_name, "--workdir", system_root + "\\System32", "--", curl]
        + curl_common + ["--output", std::path::join(scratch, "default-response.txt").display, endpoint], repo, timeout);
    require(denied.exit_code == 7, "WNET.2 expected curl rc=7, got " + denied.exit_code.to_string()); print("PASS WNET.2 default denied");
    let allowed = run(wbox, ["run", "--name", allowed_name, "--workdir", system_root + "\\System32", "--allow-network", "--", curl]
        + curl_common + ["--output", std::path::join(scratch, "allowed-response.txt").display, endpoint], repo, timeout);
    expect(allowed, "WNET.3 allowed endpoint"); print("PASS WNET.3 allowed endpoint");
} catch (error) {
    let cleanup = std::process::command(wbox); cleanup.args(["rm", "-f", default_name, allowed_name]); cleanup.timeout(std::time::Duration::from_secs(30)); cleanup.output();
    if std::fs::exists(scratch) { std::fs::remove_dir_all(scratch); }
    throw error;
}
let state = run(wbox, ["ps", "--all"], repo, 30);
expect(state, "WNET.4 state inspection");
require(!state.stdout_text().contains(default_name) && !state.stdout_text().contains(allowed_name), "WNET.4 foreground state leaked");
let cleanup = std::process::command(wbox); cleanup.args(["rm", "-f", default_name, allowed_name]); cleanup.timeout(std::time::Duration::from_secs(30)); cleanup.output();
if std::fs::exists(scratch) { std::fs::remove_dir_all(scratch); }
print("PASS WNET.4 foreground state cleanup");
