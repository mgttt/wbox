#!/usr/bin/env sh
set -eu

repo_root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
target_dir=${CARGO_TARGET_DIR:-"$repo_root/target"}
keep_incremental=${WBOX_KEEP_INCREMENTAL:-0}
clean_incremental=${WBOX_CLEAN_INCREMENTAL:-0}

if [ "$keep_incremental" = 1 ] && [ "$clean_incremental" = 1 ]; then
  printf 'target cleanup: WBOX_KEEP_INCREMENTAL and WBOX_CLEAN_INCREMENTAL conflict\n' >&2
  exit 1
fi

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
removed_files=0
removed_kib=0
if [ "$keep_incremental" != 1 ]; then
  incremental_root="$target_dir/debug/incremental"
  if [ -d "$incremental_root" ]; then
    if [ "$clean_incremental" = 1 ]; then
      size=$(du -sk "$incremental_root" | awk '{print $1}')
      removed_kib=$((removed_kib + size))
      rm -rf -- "$incremental_root"
      removed=$((removed + 1))
    else
      for crate_dir in "$incremental_root"/*; do
        [ -d "$crate_dir" ] || continue
        for lock_file in "$crate_dir"/*.lock; do
          [ -f "$lock_file" ] || continue
          session_prefix=${lock_file%.lock}-
          has_session=0
          for session_dir in "$session_prefix"*; do
            if [ -d "$session_dir" ]; then
              has_session=1
              break
            fi
          done
          if [ "$has_session" -eq 0 ]; then
            size=$(du -k "$lock_file" | awk '{print $1}')
            removed_kib=$((removed_kib + size))
            rm -f -- "$lock_file"
            removed_files=$((removed_files + 1))
          fi
        done
        if [ -z "$(find "$crate_dir" -mindepth 1 -maxdepth 1 -print -quit)" ]; then
          rmdir -- "$crate_dir"
          removed=$((removed + 1))
        fi
      done
    fi
  fi
fi

for directory in "$target_dir/tmp" "$target_dir"/review-*; do
  [ -d "$directory" ] || continue
  size=$(du -sk "$directory" | awk '{print $1}')
  removed_kib=$((removed_kib + size))
  rm -rf -- "$directory"
  removed=$((removed + 1))
done

for file in "$target_dir"/*.tmp "$target_dir"/*.part "$target_dir"/review-*; do
  [ -f "$file" ] || continue
  size=$(du -k "$file" | awk '{print $1}')
  removed_kib=$((removed_kib + size))
  rm -f -- "$file"
  removed_files=$((removed_files + 1))
done

released_mib=$((removed_kib / 1024))
printf 'target cleanup: removed %s regenerable directories and %s temporary files; released %s MiB\n' \
  "$removed" "$removed_files" "$released_mib"
