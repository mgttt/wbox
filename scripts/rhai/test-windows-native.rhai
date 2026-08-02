// Windows native process matrix. Args: WBOX executable.
fn require(condition, message) { if !condition { throw message; } }
fn invoke(wbox, label, expected, marker, argv, repo) {
    let command = std::process::command(wbox); command.args(argv); command.current_dir(repo);
    command.timeout(std::time::Duration::from_secs(60)); command.capture_limit(262144);
    let output = command.output();
    require(output.exit_code == expected, label + " expected rc=" + expected.to_string() +
        " got rc=" + output.exit_code.to_string() + "\n" + output.stdout_text() + output.stderr_text());
    if marker != "" { require(output.stdout_text().contains(marker), label + " missing marker " + marker); }
    print("PASS " + label);
    output
}
let repo = std::env::current_dir().display;
require(args.len >= 1, "expected WBOX");
let wbox = std::path::absolute(args[0]).display;
require(std::fs::exists(wbox), "wbox_not_found:" + wbox);
let system_root = std::env::get("SystemRoot");
let system32 = std::path::join(system_root, "System32").display;
let powershell = std::path::join(system32, "WindowsPowerShell/v1.0/powershell.exe").display;
let tag = std::process::id().to_string();
let names = ["wn-cmd-" + tag, "wn-ps-" + tag, "wn-host-" + tag, "wn-who-" + tag,
    "wn-child-" + tag, "wn-write-" + tag, "wn-exit-" + tag];
let sandbox = std::path::join(rhai::runtime::temp_dir().display, "wbox-native-" + tag).display;
let writable = std::path::join(sandbox, "writable").display;
std::fs::create_dir_all(writable);
try {
    let acl = std::process::command("icacls.exe"); acl.args([writable, "/grant", "*S-1-15-2-1:(OI)(CI)(M)", "/T", "/C"]); acl.timeout(std::time::Duration::from_secs(60)); require(acl.output().success, "native_acl_failed");
    invoke(wbox, "WN.1 cmd.exe", 0, "CMD_NATIVE_OK", ["run", "--name", names[0], "--workdir", system32, "--", "cmd.exe", "/d", "/c", "echo CMD_NATIVE_OK"], repo);
    invoke(wbox, "WN.2 Windows PowerShell", 0, "POWERSHELL_NATIVE_OK:42", ["run", "--name", names[1], "--workdir", system32, "--", powershell, "-NoLogo", "-NoProfile", "-NonInteractive", "-Command", "[Console]::WriteLine('POWERSHELL_NATIVE_OK:' + (6 * 7))"], repo);
    invoke(wbox, "WN.3 hostname.exe", 0, "", ["run", "--name", names[2], "--workdir", system32, "--", "hostname.exe"], repo);
    invoke(wbox, "WN.4 whoami.exe", 0, "", ["run", "--name", names[3], "--workdir", system32, "--", "whoami.exe"], repo);
    invoke(wbox, "WN.5 child process", 0, "CHILD_PROCESS_OK", ["run", "--name", names[4], "--workdir", system32, "--", "cmd.exe", "/d", "/c", "cmd.exe /d /c echo CHILD_PROCESS_OK"], repo);
    invoke(wbox, "WN.6 writable workdir", 0, "", ["run", "--name", names[5], "--workdir", writable, "--", "cmd.exe", "/d", "/c", "echo WRITE_OK> sandbox-write.txt"], repo);
    let written = std::fs::read_to_string(std::path::join(writable, "sandbox-write.txt").display); require(written.trim() == "WRITE_OK", "WN.6 unexpected content:" + written);
    invoke(wbox, "WN.7 exit-code forwarding", 23, "", ["run", "--name", names[6], "--workdir", system32, "--", "cmd.exe", "/d", "/c", "exit /b 23"], repo);
    let state = invoke(wbox, "WN.8 state inspection", 0, "", ["ps", "--all"], repo).stdout_text();
    for name in names { require(!state.contains(name), "WN.8 leaked state:" + name); }
    print("PASS WN.8 foreground state cleanup");
} catch (error) {
    let cleanup = std::process::command(wbox); cleanup.args(["rm", "-f"] + names); cleanup.timeout(std::time::Duration::from_secs(60)); cleanup.output();
    if std::fs::exists(sandbox) { std::fs::remove_dir_all(sandbox); }
    throw error;
}
let cleanup = std::process::command(wbox); cleanup.args(["rm", "-f"] + names); cleanup.timeout(std::time::Duration::from_secs(60)); cleanup.output();
if std::fs::exists(sandbox) { std::fs::remove_dir_all(sandbox); }
