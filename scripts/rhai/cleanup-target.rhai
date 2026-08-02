// Cargo target cleanup with explicit Cargo-cache safety checks.
// Args: [--target-dir PATH] [--keep-incremental] [--clean-incremental]
//       [--cargo-finished] [--max-incremental-mib N]

fn require(condition, message) { if !condition { throw message; } }
fn value(argv, name, fallback) {
    let index = argv.index_of(name);
    if index < 0 { return fallback; }
    require(index + 1 < argv.len, "cleanup_option_missing:" + name);
    argv[index + 1]
}
fn has(argv, name) { argv.contains(name) }
fn direct(path) {
    std::fs::exists(path) && {
        let metadata = std::fs::symlink_metadata(path);
        metadata.is_file || metadata.is_dir
    }
}
fn child_of(root, path) {
    let r = root.to_lower();
    let p = path.to_lower();
    p.starts_with(r + "\\") || p.starts_with(r + "/")
}
fn remove_entry(root, path) {
    require(child_of(root, path), "cleanup_outside_target:" + path);
    let metadata = std::fs::symlink_metadata(path);
    if metadata.is_symlink || metadata.is_reparse_point {
        std::fs::remove_file(path);
    } else if metadata.is_dir {
        std::fs::remove_dir_all(path);
    } else {
        std::fs::remove_file(path);
    }
}

let cli_args = args;
let repo = std::env::current_dir().display;
let configured_target = if std::env::has("CARGO_TARGET_DIR") {
    std::env::get("CARGO_TARGET_DIR")
} else { "" };
let requested = value(cli_args, "--target-dir", configured_target);
let target = if requested == "" { std::path::join(repo, "target").display }
    else { std::path::absolute(requested).display };
if !direct(target) {
    print("target cleanup: nothing to clean at " + target);
    return;
}
let tag = std::path::join(target, "CACHEDIR.TAG").display;
require(direct(tag) && std::fs::read_to_string(tag).contains("created by cargo"),
    "cleanup_refusing_non_cargo_target:" + target);
let incremental = std::path::join(target, "debug/incremental").display;
let removed = 0;
if has(cli_args, "--keep-incremental") && has(cli_args, "--clean-incremental") {
    throw "cleanup_incremental_options_conflict";
}
if !has(cli_args, "--keep-incremental") && direct(incremental) {
    if has(cli_args, "--clean-incremental") {
        remove_entry(target, incremental);
        removed += 1;
    } else {
        for crate_entry in std::fs::read_dir(incremental) {
            if !crate_entry.is_dir { continue; }
            let crate_path = crate_entry.path.display;
            for session in std::fs::read_dir(crate_path) {
                if !session.is_dir || !session.file_name.ends_with("-working") { continue; }
                if has(cli_args, "--cargo-finished") { remove_entry(target, session.path.display); removed += 1; }
            }
        }
    }
}
for entry in std::fs::read_dir(target) {
    if !entry.is_dir { continue; }
    if entry.file_name == "tmp" || entry.file_name.starts_with("review-") {
        remove_entry(target, entry.path.display); removed += 1;
    }
}
for entry in std::fs::read_dir(target) {
    if entry.is_dir || !(entry.file_name.ends_with(".tmp")
        || entry.file_name.ends_with(".part") || entry.file_name.starts_with("review-")) {
        continue;
    }
    remove_entry(target, entry.path.display); removed += 1;
}
print("target cleanup: PASS removed_entries=" + removed.to_string() + " root=" + target);
