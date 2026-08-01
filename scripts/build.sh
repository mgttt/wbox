#!/usr/bin/env sh
set -u

script_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
repo_root=$(CDPATH= cd -- "$script_dir/.." && pwd)
build_status=0

cd "$repo_root"
cargo build --locked "$@" || build_status=$?
WBOX_CARGO_FINISHED=1 "$script_dir/cleanup-target.sh" || cleanup_status=$?

if [ "$build_status" -ne 0 ]; then
  exit "$build_status"
fi
exit "${cleanup_status:-0}"
