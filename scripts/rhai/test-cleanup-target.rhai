// Cleanup safety/fixture test. The cleanup implementation is itself Rhai.
fn require(condition, message) { if !condition { throw message; } }
let repo = std::env::current_dir().display;
let fixture = std::path::join(rhai::runtime::temp_dir().display,
    "wbox-cleanup-" + std::process::id()).display;
let target = std::path::join(fixture, "debug/incremental").display;
let runtime = if std::env::has("AGENTERM_SCRIPT_EXE") {
    std::env::get("AGENTERM_SCRIPT_EXE")
} else { "agenterm-script" };
try {
    std::fs::create_dir_all(target);
    std::fs::write(std::path::join(fixture, "CACHEDIR.TAG").display,
        "Signature: 8a477f597d28d172789f06886806bc55\n# created by cargo.\n");
    std::fs::create_dir_all(std::path::join(target, "crate-a/s-old-working").display);
    std::fs::write(std::path::join(target, "crate-a/s-old-working/payload.bin").display, "fixture");
    let command = std::process::command(runtime);
    command.args(["run", std::path::join(repo, "scripts/rhai/cleanup-target.rhai").display,
        "--cwd", repo, "--project-root", repo, "--timeout-ms", "60000", "--",
        "--target-dir", fixture, "--cargo-finished"]);
    command.current_dir(repo); command.timeout(std::time::Duration::from_secs(90)); command.capture_limit(262144);
    let output = command.output();
    require(output.success, "cleanup_fixture_failed:\n" + output.stdout_text() + output.stderr_text());
    require(!std::fs::exists(std::path::join(target, "crate-a/s-old-working").display), "working_session_survived");
    print("PASS cleanup target fixture");
} catch (error) {
    if std::fs::exists(fixture) { std::fs::remove_dir_all(fixture); }
    throw error;
}
if std::fs::exists(fixture) { std::fs::remove_dir_all(fixture); }
