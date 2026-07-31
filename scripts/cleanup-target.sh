#!/usr/bin/env sh
set -eu

repo_root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
target_dir=${CARGO_TARGET_DIR:-"$repo_root/target"}
keep_incremental=${WBOX_KEEP_INCREMENTAL:-0}

case "$target_dir" in
  /*) ;;
  *) target_dir="$repo_root/$target_dir" ;;
esac

if [ ! -d "$target_dir" ]; then
  printf 'target cleanup: nothing to clean at %s\n' "$target_dir"
  exit 0
fi

cache_tag="$target_dir/CACHEDIR.TAG"
if [ ! -f "$cache_tag" ] || ! grep -q 'created by cargo' "$cache_tag"; then
  printf "target cleanup: refusing to clean '%s': Cargo CACHEDIR.TAG is missing\n" \
    "$target_dir" >&2
  exit 1
fi

removed=0
if [ "$keep_incremental" != 1 ]; then
  incremental_count=$(
    find "$target_dir" -type d -name incremental -prune -print |
      wc -l |
      tr -d ' '
  )
  find "$target_dir" -type d -name incremental -prune -exec rm -rf -- {} +
  removed=$((removed + incremental_count))
fi

for directory in "$target_dir/tmp" "$target_dir"/review-*; do
  [ -d "$directory" ] || continue
  rm -rf -- "$directory"
  removed=$((removed + 1))
done

removed_files=0
for file in "$target_dir"/*.tmp "$target_dir"/*.part "$target_dir"/review-*; do
  [ -f "$file" ] || continue
  rm -f -- "$file"
  removed_files=$((removed_files + 1))
done

printf 'target cleanup: removed %s regenerable directories and %s temporary files\n' \
  "$removed" "$removed_files"
