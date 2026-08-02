// Native build entry point. Args: [--release] [--package NAME] [--target TRIPLE]
// Remaining arguments are forwarded to Cargo.

fn fail(output, code) {
    if !output.success {
        throw code + " (exit=" + output.exit_code.to_string() + ")\n"
            + output.stdout_text() + output.stderr_text();
    }
}

let repo = std::env::current_dir().display;
let cargo_args = ["build", "--locked"];
let index = 0;
while index < args.len {
    let value = args[index];
    if value == "--" {
        index += 1;
        continue;
    }
    if value != "--keep-incremental" && value != "--clean-incremental" {
        cargo_args.push(value);
    }
    index += 1;
}
let command = std::process::command("cargo");
command.args(cargo_args);
command.current_dir(repo);
command.timeout(std::time::Duration::from_secs(1800));
command.capture_limit(262144);
let output = command.output();
fail(output, "build_cargo");
print("build: PASS");
