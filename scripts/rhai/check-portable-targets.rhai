// Cross-target Clippy checks. Args are Rust target triples.

fn require(condition, message) {
    if !condition { throw message; }
}

let repo = std::env::current_dir().display;
let installed_command = std::process::command("rustup");
installed_command.args(["target", "list", "--installed"]);
installed_command.current_dir(repo);
installed_command.timeout(std::time::Duration::from_secs(30));
let installed_output = installed_command.output();
require(installed_output.success, "rustup_target_list_failed");
let installed = installed_output.stdout_text().split("\n");
let targets = if args.len == 0 {
    ["i686-pc-windows-msvc", "aarch64-unknown-linux-gnu",
        "x86_64-apple-darwin", "aarch64-apple-darwin"]
} else { args };
for target in targets {
    require(installed.contains(target), "rust_target_not_installed:" + target);
    print("portable target: " + target);
    let command = std::process::command("cargo");
    command.args(["clippy", "--locked", "--workspace", "--all-targets",
        "--target", target, "--message-format", "short", "--", "-D", "warnings"]);
    command.current_dir(repo);
    command.env("CARGO_INCREMENTAL", "0");
    command.timeout(std::time::Duration::from_secs(1800));
    command.capture_limit(262144);
    let output = command.output();
    require(output.success, "portable_target_failed:" + target + "\n"
        + output.stdout_text() + output.stderr_text());
}
print("portable targets: PASS");
