// WP.8–WP.12 supervisor/guest/child stop contract. Args: WBOX BUSYBOX.
fn require(condition, message) { if !condition { throw message; } }
fn run(program, argv, cwd, timeout, env) {
    let command = std::process::command(program); command.args(argv); command.current_dir(cwd);
    for key in env.keys() { command.env(key, env[key]); }
    command.timeout(std::time::Duration::from_secs(timeout)); command.capture_limit(262144); command.output()
}
fn process_present(pid) { for process in std::process::list() { if process.id == pid { return true; } } false }
fn wait_pid(path, timeout_ms) {
    for attempt in 0..(timeout_ms / 100) { if std::fs::exists(path) { let value = std::fs::read_to_string(path).trim().parse_int(); if value > 0 { return value; } } rhai::task::sleep(std::time::Duration::from_millis(100)); }
    throw "pid_file_timeout:" + path;
}
fn ok(output, label) { require(output.success, label + " failed:\n" + output.stdout_text() + output.stderr_text()); }
fn text(output) { output.stdout_text() + output.stderr_text() }
let repo = std::env::current_dir().display;
require(args.len >= 2, "expected WBOX BUSYBOX");
let wbox = std::path::absolute(args[0]).display; let busybox = std::path::absolute(args[1]).display;
let tag = std::process::id().to_string(); let sandbox = std::path::join(rhai::runtime::temp_dir().display, "wbox-stop-" + tag).display; let home = std::path::join(sandbox, "home").display; let image = std::path::join(home, ".wbox/images/local.test/wbox-fixture/latest").display; let rootfs = std::path::join(image, "rootfs").display; let work = std::path::join(sandbox, "stop-work").display;
let system_root = std::env::get("SystemRoot"); let system32 = std::path::join(system_root, "System32").display; let env = #{USERPROFILE: home, HOME: home}; let name = "product-stop-" + tag;
let guest_pid_path = std::path::join(work, "guest.pid").display; let child_pid_path = std::path::join(work, "child.pid").display; let workload = std::path::join(work, "workload.ps1").display;
try {
    std::fs::create_dir_all(rootfs); std::fs::create_dir_all(work); std::fs::copy(busybox, std::path::join(rootfs, "busybox").display); std::fs::write(std::path::join(image, "manifest.json").display, "{}"); std::fs::write(std::path::join(image, "layers.json").display, "[]"); std::fs::write(std::path::join(image, "config.json").display, `{"config":{"Entrypoint":[],"Cmd":["/busybox","true"],"Env":["PATH=/"],"WorkingDir":"/"}}`);
    let acl = std::process::command("icacls.exe"); acl.args([work, "/grant", "*S-1-15-2-1:(OI)(CI)(M)", "/T", "/C"]); acl.timeout(std::time::Duration::from_secs(60)); require(acl.output().success, "WP.8 ACL");
    std::fs::write(workload, `[IO.File]::WriteAllText('guest.pid',[string]$PID)
$child=[Diagnostics.Process]::Start((New-Object Diagnostics.ProcessStartInfo -Property @{FileName="$env:SystemRoot\System32\WindowsPowerShell\v1.0\powershell.exe";Arguments='-NoLogo -NoProfile -NonInteractive -Command "[Threading.Thread]::Sleep(3600000)"';UseShellExecute=$false;CreateNoWindow=$true})
[IO.File]::WriteAllText('child.pid',[string]$child.Id)
while($true){[Threading.Thread]::Sleep(60000)}`);
    let command_script = `[IO.File]::WriteAllText('` + guest_pid_path + `',[string]$PID);$child=[Diagnostics.Process]::Start((New-Object Diagnostics.ProcessStartInfo -Property @{FileName="$env:SystemRoot\System32\WindowsPowerShell\v1.0\powershell.exe";Arguments='-NoLogo -NoProfile -NonInteractive -Command "[Threading.Thread]::Sleep(3600000)"';UseShellExecute=$false;CreateNoWindow=$true}));[IO.File]::WriteAllText('` + child_pid_path + `',[string]$child.Id);[Threading.Thread]::Sleep(3600000)`;
    let launch = run(wbox, ["run", "-d", "--name", name, "--workdir", work, "--", "powershell.exe", "-NoLogo", "-NoProfile", "-NonInteractive", "-Command", command_script], repo, 60, env); ok(launch, "WP.8 detached launch"); let launch_text = text(launch); launch_text.trim(); require(launch_text == name, "WP.8 launch output");
    let guest = 0; try { guest = wait_pid(guest_pid_path, 15000); } catch (error) { let logs = run(wbox, ["logs", name], repo, 30, env); throw error.to_string() + " logs=" + text(logs); }
    let child = wait_pid(child_pid_path, 15000); let meta = std::path::join(home, ".wbox/run/" + name + "/meta.json").display; let supervisor = rhai::json::parse(std::fs::read_to_string(meta)).pid; require(guest != child && guest != supervisor && child != supervisor, "WP.8 PIDs not distinct"); require(process_present(guest) && process_present(child) && process_present(supervisor), "WP.8 process tree not alive"); print("PASS WP.8 supervisor -> guest -> child tree");
    let top = run(wbox, ["top", name], repo, 60, env); ok(top, "WP.19 top"); require(top.stdout_text().contains(guest.to_string()) && top.stdout_text().contains(child.to_string()) && !top.stdout_text().contains(supervisor.to_string()), "WP.19 top membership"); print("PASS WP.19 top lists Job members");
    ok(run(wbox, ["stop", name], repo, 60, env), "WP.9 stop"); for attempt in 0..150 { if !process_present(guest) && !process_present(child) && !process_present(supervisor) { break; } rhai::task::sleep(std::time::Duration::from_millis(100)); } require(!process_present(guest) && !process_present(child) && !process_present(supervisor), "WP.9 processes survived stop"); print("PASS WP.9 stop reclaims process tree");
    ok(run(wbox, ["stop", name], repo, 60, env), "WP.10 idempotent stop"); print("PASS WP.10 idempotent stop"); let missing = run(wbox, ["stop", name + "-missing"], repo, 60, env); require(!missing.success, "WP.11 unknown stop succeeded"); print("PASS WP.11 unknown stop rejected"); ok(run(wbox, ["rm", name], repo, 60, env), "WP.12 rm"); let state = run(wbox, ["ps", "--all"], repo, 60, env); ok(state, "WP.12 state"); require(!state.stdout_text().contains(name), "WP.12 record survived rm"); print("PASS WP.12 rm cleans stopped record");
} catch (error) { let cleanup = std::process::command(wbox); cleanup.args(["rm", "-f", name]); cleanup.timeout(std::time::Duration::from_secs(60)); cleanup.output(); if std::fs::exists(sandbox) { try { std::fs::remove_dir_all(sandbox); } catch (cleanup_error) {} } throw error; }
let cleanup = std::process::command(wbox); cleanup.args(["rm", "-f", name]); cleanup.timeout(std::time::Duration::from_secs(60)); cleanup.output(); if std::fs::exists(sandbox) { try { std::fs::remove_dir_all(sandbox); } catch (cleanup_error) {} }
