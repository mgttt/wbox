// Cross-platform repository lint entry point.
// Args: [All|Static|Rust] [--windows-target]

fn require_ok(output, code) {
    if !output.success {
        throw code + " (exit=" + output.exit_code.to_string() + ")\n"
            + output.stdout_text() + output.stderr_text();
    }
}

fn run(program, arguments, cwd, timeout_seconds, code) {
    let command = std::process::command(program);
    command.args(arguments);
    command.current_dir(cwd);
    command.timeout(std::time::Duration::from_secs(timeout_seconds));
    command.capture_limit(262144);
    let output = command.output();
    require_ok(output, code);
    output.stdout_text()
}

fn repo_files(repo, pattern) {
    let output = run("git", ["-C", repo, "ls-files", "--cached", "--others",
        "--exclude-standard", "--", pattern], repo, 10, "lint_git_ls_files");
    let paths = [];
    for line in output.split("\n") {
        let relative = line;
        relative.trim();
        if relative != "" {
            paths.push(std::path::join(repo, relative).display);
        }
    }
    paths
}

fn static_lint(repo) {
    run("git", ["-C", repo, "diff", "--check"], repo, 10, "lint_diff");
    run("git", ["-C", repo, "diff", "--cached", "--check"], repo, 10, "lint_cached_diff");
    for path in repo_files(repo, "*.json") {
        rhai::json::parse_file(path);
    }
    let migrated = [
        "lint", "build", "check", "check-portable-targets", "cleanup-target",
        "probe-windows-virtualstore", "test-cleanup-target", "test-windows-native",
        "test-windows-network", "test-windows-product", "test-windows-ubuntu"
    ];
    for stem in migrated {
        let script = std::path::join(repo, "scripts/rhai/" + stem + ".rhai").display;
        if !std::fs::exists(script) {
            throw "rhai_migration_missing:" + script;
        }
    }
}

fn rust_lint(repo, windows_target) {
    run("cargo", ["fmt", "--all", "--", "--check"], repo, 60, "lint_rustfmt");
    let arguments = ["clippy", "--locked", "--workspace", "--all-targets",
        "--message-format", "short", "--", "-D", "warnings"];
    run("cargo", arguments, repo, 120, "lint_clippy_host");
    if windows_target {
        run("cargo", ["clippy", "--locked", "--workspace", "--all-targets",
            "--target", "x86_64-pc-windows-msvc", "--message-format", "short",
            "--", "-D", "warnings"], repo, 180, "lint_clippy_windows");
    }
}

let repo = std::env::current_dir().display;
let mode = if args.len > 0 { args[0] } else { "All" };
let windows_target = args.contains("--windows-target");
if mode == "All" || mode == "Static" {
    static_lint(repo);
}
if mode == "All" || mode == "Rust" {
    rust_lint(repo, windows_target);
}
if mode != "All" && mode != "Static" && mode != "Rust" {
    throw "lint mode must be All, Static, or Rust";
}
print("lint: " + mode + " PASS");
