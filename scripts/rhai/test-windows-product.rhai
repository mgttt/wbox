// Product migration orchestrator for the migrated WP.1–WP.7 journeys.
// Args: WBOX WBOX_LINUX BUSYBOX.
fn require_ok(output, code) { if !output.success { throw code + "\n" + output.stdout_text() + output.stderr_text(); } }
let repo = std::env::current_dir().display;
if args.len < 3 { throw "product_orchestrator_args"; }
let runtime = if std::env::has("AGENTERM_SCRIPT_EXE") { std::env::get("AGENTERM_SCRIPT_EXE") } else { "agenterm-script" };
let core = std::process::command(runtime);
core.args(["run", std::path::join(repo, "scripts/rhai/test-windows-product-core.rhai").display,
    "--cwd", repo, "--project-root", repo, "--timeout-ms", "3600000", "--",
    args[0], args[1], args[2]]);
core.current_dir(repo); core.timeout(std::time::Duration::from_secs(3600)); core.capture_limit(262144);
let core_output = core.output(); require_ok(core_output, "product_core"); print(core_output.stdout_text());
let lifecycle = std::process::command(runtime);
lifecycle.args(["run", std::path::join(repo, "scripts/rhai/test-windows-product-lifecycle.rhai").display,
    "--cwd", repo, "--project-root", repo, "--timeout-ms", "3600000", "--", args[0], args[2]]);
lifecycle.current_dir(repo); lifecycle.timeout(std::time::Duration::from_secs(3600)); lifecycle.capture_limit(262144);
let lifecycle_output = lifecycle.output(); require_ok(lifecycle_output, "product_lifecycle"); print(lifecycle_output.stdout_text());
let stop = std::process::command(runtime);
stop.args(["run", std::path::join(repo, "scripts/rhai/test-windows-product-stop.rhai").display,
    "--cwd", repo, "--project-root", repo, "--timeout-ms", "3600000", "--", args[0], args[2]]);
stop.current_dir(repo); stop.timeout(std::time::Duration::from_secs(3600)); stop.capture_limit(262144);
let stop_output = stop.output(); require_ok(stop_output, "product_stop"); print(stop_output.stdout_text());
let exec = std::process::command(runtime);
exec.args(["run", std::path::join(repo, "scripts/rhai/test-windows-product-exec.rhai").display,
    "--cwd", repo, "--project-root", repo, "--timeout-ms", "3600000", "--", args[0], args[2]]);
exec.current_dir(repo); exec.timeout(std::time::Duration::from_secs(3600)); exec.capture_limit(262144);
let exec_output = exec.output(); require_ok(exec_output, "product_exec"); print(exec_output.stdout_text());
let crash = std::process::command(runtime);
crash.args(["run", std::path::join(repo, "scripts/rhai/test-windows-product-crash.rhai").display,
    "--cwd", repo, "--project-root", repo, "--timeout-ms", "3600000", "--", args[0], args[2]]);
crash.current_dir(repo); crash.timeout(std::time::Duration::from_secs(3600)); crash.capture_limit(262144);
let crash_output = crash.output(); require_ok(crash_output, "product_crash"); print(crash_output.stdout_text());
let build = std::process::command(runtime);
build.args(["run", std::path::join(repo, "scripts/rhai/test-windows-product-build.rhai").display,
    "--cwd", repo, "--project-root", repo, "--timeout-ms", "3600000", "--", args[0], args[2]]);
build.current_dir(repo); build.timeout(std::time::Duration::from_secs(3600)); build.capture_limit(262144);
let build_output = build.output(); require_ok(build_output, "product_build"); print(build_output.stdout_text());
let kill_fail = std::process::command(runtime);
kill_fail.args(["run", std::path::join(repo, "scripts/rhai/test-windows-product-kill-fail.rhai").display,
    "--cwd", repo, "--project-root", repo, "--timeout-ms", "3600000", "--", args[0], args[2]]);
kill_fail.current_dir(repo); kill_fail.timeout(std::time::Duration::from_secs(3600)); kill_fail.capture_limit(262144);
let kill_output = kill_fail.output(); require_ok(kill_output, "product_kill_fail"); print(kill_output.stdout_text());
let env_journey = std::process::command(runtime);
env_journey.args(["run", std::path::join(repo, "scripts/rhai/test-windows-product-env.rhai").display,
    "--cwd", repo, "--project-root", repo, "--timeout-ms", "3600000", "--", args[0], args[2]]);
env_journey.current_dir(repo); env_journey.timeout(std::time::Duration::from_secs(3600)); env_journey.capture_limit(262144);
let env_output = env_journey.output(); require_ok(env_output, "product_env"); print(env_output.stdout_text());
let create = std::process::command(runtime);
create.args(["run", std::path::join(repo, "scripts/rhai/test-windows-product-create.rhai").display,
    "--cwd", repo, "--project-root", repo, "--timeout-ms", "3600000", "--", args[0], args[2]]);
create.current_dir(repo); create.timeout(std::time::Duration::from_secs(3600)); create.capture_limit(262144);
let create_output = create.output(); require_ok(create_output, "product_create"); print(create_output.stdout_text());
print("Windows product migrated WP.1-WP.27 PASS");
