#!/usr/bin/env bash
# Real installs occur only in disposable Docker containers; host needs Docker,
# qemu-user-static/binfmt (on x86), Rust's ARM target and a cross linker.
set -euo pipefail
REPO=$(cd "$(dirname "$0")/.." && pwd)
OUT=${1:-$REPO/.tools/p3-qemu-$(date -u +%Y%m%dT%H%M%SZ)}
mkdir -p "$OUT"
OUT=$(cd "$OUT" && pwd)
read -r -a docker_cmd <<< "${DOCKER:-docker}"
command -v "${docker_cmd[0]}" >/dev/null
[[ $(uname -m) == x86_64 ]] || { echo 'Run the cross-platform suite on x86_64'; exit 2; }
[[ ! -e $OUT/main.repos ]] || { echo "Use a new output directory: $OUT"; exit 2; }
exec > >(tee "$OUT/suite.log") 2>&1
trap 'echo "P3 failed at line $LINENO; inspect logs in $OUT"' ERR
# Freeze scripts and fixtures: editing the checkout during a long QEMU run
# must not alter an executing Bash script or the source being accepted.
cp -R "$REPO/scripts/p3" "$OUT/suite"
cargo build --locked -p rospm-cli --manifest-path "$REPO/Cargo.toml"
CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_LINKER=${CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_LINKER:-aarch64-linux-gnu-gcc} \
    cargo build --locked -p rospm-cli --manifest-path "$REPO/Cargo.toml" --target aarch64-unknown-linux-gnu
cp "$REPO/target/debug/rospm" "$OUT/rospm-x86_64"
cp "$REPO/target/aarch64-unknown-linux-gnu/debug/rospm" "$OUT/rospm-aarch64"
if [[ -n ${P3_ARM_SEED:-} ]]; then
    # Reuse only real, previously collected ARM pins, with the same binaries
    # and exact fixture. Always replay into a fresh ARM container below.
    P3_ARM_SEED=$(cd "$P3_ARM_SEED" && pwd)
    cmp "$OUT/rospm-x86_64" "$P3_ARM_SEED/rospm-x86_64"
    cmp "$OUT/rospm-aarch64" "$P3_ARM_SEED/rospm-aarch64"
    cp -R "$P3_ARM_SEED/upstream" "$OUT/upstream"
    for file in package.xml CMakeLists.txt src/probe.cpp; do
        cmp "$OUT/upstream/$file" "$OUT/suite/fixture/$file"
    done
    git -C "$OUT/upstream" diff --exit-code HEAD --
    cp "$P3_ARM_SEED/aarch64.lock" "$OUT/aarch64.lock"
    mkdir "$OUT/seed-arm-evidence"
    cp "$P3_ARM_SEED/"seed-aarch64-*.log "$OUT/seed-arm-evidence/"
    ROS_SNAPSHOT=$(sed -n 's/^ros_snapshot = "\([^"]*\)"/\1/p' "$OUT/aarch64.lock")
    echo "Reusing collected ARM seed: $P3_ARM_SEED"
else
    mkdir -p "$OUT/upstream/src"
    cp "$OUT/suite/fixture/"{package.xml,CMakeLists.txt} "$OUT/upstream/"
    cp "$OUT/suite/fixture/src/probe.cpp" "$OUT/upstream/src/"
    git -C "$OUT/upstream" init -q -b main
    git -C "$OUT/upstream" add .
    git -C "$OUT/upstream" -c user.name='rospm P3' -c user.email=test@example.com commit -qm 'Native ARM64 acceptance fixture'
fi
git -C "$OUT/upstream" rev-parse HEAD > "$OUT/source.rev"
if [[ -z ${ROS_SNAPSHOT:-} ]]; then
    ROS_SNAPSHOT=$(curl -fsSL --retry 3 --retry-all-errors --connect-timeout 15 --max-time 60 http://snapshots.ros.org/jazzy/ | \
        sed -n 's/.*href="\([0-9]\{4\}-[0-9]\{2\}-[0-9]\{2\}\)\/".*/\1/p' | sort | tail -1)
fi
[[ $ROS_SNAPSHOT =~ ^[0-9]{4}-[0-9]{2}-[0-9]{2}$ ]]
export ROS_SNAPSHOT
cat > "$OUT/main.repos" <<'REPOS'
repositories:
  p3_native:
    type: git
    url: /upstream
    version: main
REPOS
for arch in amd64 arm64; do
    "${docker_cmd[@]}" build --platform "linux/$arch" -t "rospm-p3:$arch" \
        -f "$OUT/suite/Dockerfile" "$OUT/suite" > "$OUT/image-$arch.log" 2>&1
done
prefix=rospm-p3-$$
cleanup() { "${docker_cmd[@]}" rm -f "$prefix-x86" "$prefix-arm" "$prefix-replay" >/dev/null 2>&1 || true; }
trap cleanup EXIT
container() {
    local name=$1 platform=$2 binary=$3
    "${docker_cmd[@]}" run -d --name "$prefix-$name" --platform "linux/$platform" \
        -e "ROS_SNAPSHOT=$ROS_SNAPSHOT" -v "$OUT/suite:/suite:ro" -v "$OUT:/exchange" \
        -v "$OUT/upstream:/upstream:ro" -v "$OUT/rospm-$binary:/rospm:ro" \
        "rospm-p3:$platform" sleep infinity >/dev/null
}
{
    date -u --iso-8601=seconds
    git -C "$REPO" rev-parse HEAD
    uname -a
    rustc --version
    "${docker_cmd[@]}" version
    qemu-aarch64-static --version
    cat /proc/sys/fs/binfmt_misc/qemu-aarch64
    "${docker_cmd[@]}" image inspect rospm-p3:amd64 rospm-p3:arm64 --format '{{.Id}} {{.Architecture}}'
    sha256sum "$OUT/rospm-x86_64" "$OUT/rospm-aarch64" "$OUT/suite/guest.sh" "$OUT/suite/validate.py"
    echo "ROS snapshot: $ROS_SNAPSHOT"
    echo "ARM seed: ${P3_ARM_SEED:-collected in this run}"
} > "$OUT/environment.txt"
container x86 amd64 x86_64
"${docker_cmd[@]}" exec "$prefix-x86" bash /suite/guest.sh seed
if [[ -n ${P3_ARM_SEED:-} ]]; then
    "${docker_cmd[@]}" exec "$prefix-x86" python3 /suite/validate.py lock /exchange/aarch64.lock both
    "${docker_cmd[@]}" exec "$prefix-x86" python3 /suite/validate.py seed /exchange/aarch64.lock /suite/fixture/rospm.toml /exchange/source.rev
else
    container arm arm64 aarch64
    "${docker_cmd[@]}" exec "$prefix-arm" bash /suite/guest.sh seed
fi
"${docker_cmd[@]}" exec "$prefix-x86" bash /suite/guest.sh finalize
container replay arm64 aarch64
"${docker_cmd[@]}" exec "$prefix-replay" bash /suite/guest.sh replay
printf 'P3 QEMU PASS: %s\n' "$OUT" | tee "$OUT/result.txt"
