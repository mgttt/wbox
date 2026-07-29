#!/usr/bin/env bash
set -euo pipefail

readonly IMAGE='ubuntu@sha256:52df9b1ee71626e0088f7d400d5c6b5f7bb916f8f0c82b474289a4ece6cf3faf'
readonly DEST="${1:?usage: build-ubuntu-fixture.sh DEST}"
readonly SOURCE="${DEST}.source"

container=''
cleanup() {
    if [[ -n "$container" ]]; then
        docker rm -f "$container" >/dev/null 2>&1 || true
    fi
    rm -rf "$SOURCE"
}
trap cleanup EXIT

rm -rf "$DEST" "$SOURCE"
mkdir -p "$DEST/rootfs" "$SOURCE"

docker pull "$IMAGE"
container="$(docker create "$IMAGE")"
docker export "$container" | tar -xf - -C "$SOURCE"

copy_path() {
    local path="$1"
    if [[ ! -e "$SOURCE$path" ]]; then
        echo "missing Ubuntu fixture path: $path" >&2
        exit 1
    fi
    mkdir -p "$DEST/rootfs$(dirname "$path")"
    cp -L --preserve=mode,timestamps "$SOURCE$path" "$DEST/rootfs$path"
}

commands=(
    /bin/bash
    /usr/bin/apt
    /usr/bin/dpkg
    /usr/bin/getconf
    /usr/bin/uname
)

for command in "${commands[@]}"; do
    copy_path "$command"
done
copy_path /etc/os-release
copy_path /etc/ld.so.cache

while IFS= read -r library; do
    [[ -n "$library" ]] && copy_path "$library"
done < <(
    for command in "${commands[@]}"; do
        sudo chroot "$SOURCE" /usr/bin/ldd "$command"
    done |
        awk '
            /=> \// { print $3 }
            /^[[:space:]]*\// { sub(/^[[:space:]]*/, "", $1); print $1 }
        ' |
        sort -u
)

cat >"$DEST/config.json" <<'EOF'
{"architecture":"amd64","os":"linux","config":{"Entrypoint":[],"Cmd":["/bin/bash"],"Env":["PATH=/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin"],"WorkingDir":"/"}}
EOF
printf '{}\n' >"$DEST/manifest.json"
printf '[]\n' >"$DEST/layers.json"
cat >"$DEST/fixture.json" <<EOF
{"source":"$IMAGE","os":"linux","architecture":"amd64"}
EOF

sudo chroot "$DEST/rootfs" /bin/bash -c '
set -eu
. /etc/os-release
test "$ID" = ubuntu
test "$VERSION_ID" = 24.04
test "$(/usr/bin/uname -m)" = x86_64
case "$(/usr/bin/apt --version)" in
    "apt 2.8.3 (amd64)"*) ;;
    *) exit 1 ;;
esac
test "$(/usr/bin/dpkg --print-architecture)" = amd64
test "$(/usr/bin/getconf LONG_BIT)" = 64
'

echo "Ubuntu fixture ready: $DEST"
