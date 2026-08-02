// Repository check entry point. Args: --quick [--windows-target]

fn require_ok(output, code) {
    if !output.success {
        throw code + " (exit=" + output.exit_code.to_string() + ")\n"
            + output.stdout_text() + output.stderr_text();
    }
}

fn run(program, arguments, repo, timeout_seconds, code) {
    let command = std::process::command(program);
    command.args(arguments);
    command.current_dir(repo);
    command.timeout(std::time::Duration::from_secs(timeout_seconds));
    command.capture_limit(262144);
    let output = command.output();
    require_ok(output, code);
    if output.stdout_text() != "" { print(output.stdout_text()); }
}

let repo = std::env::current_dir().display;
let quick = args.contains("--quick");
run("git", ["diff", "--check"], repo, 30, "check_diff");
run("git", ["diff", "--cached", "--check"], repo, 30, "check_cached_diff");
run("cargo", ["fmt", "--all", "--", "--check"], repo, 120, "check_rustfmt");
let clippy = ["clippy", "--locked", "--workspace", "--all-targets",
    "--message-format", "short", "--", "-D", "warnings"];
run("cargo", clippy, repo, 1800, "check_clippy");
let test_args = if quick { ["test", "--locked", "--workspace", "--lib", "--quiet"] }
    else { ["test", "--locked", "--workspace", "--quiet"] };
run("cargo", test_args, repo, 1800, "check_tests");
print(if quick { "check: quick PASS" } else { "check: full PASS" });
