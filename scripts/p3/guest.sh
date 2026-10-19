#!/usr/bin/env bash
# Runs as root inside a disposable Ubuntu container (native still uses sudo).
set -euo pipefail
stage=${1:?seed|finalize|replay}
arch=$(uname -m)
export ROSPM_LANG=en ROS_DOMAIN_ID=73 PYTHONUNBUFFERED=1
mkdir -p /home/tester/project
cd /home/tester/project
# The controlled fixture is bind-mounted from the invoking host user.
git config --global --add safe.directory /upstream
git config --global --add safe.directory /upstream/.git
cp /suite/fixture/rospm.toml .
cp /exchange/main.repos .
rospm() { /rospm --manifest "$PWD/rospm.toml" "$@"; }
run() {
    local label=$1
    shift
    echo "P3_STEP $stage $arch $label"
    local status
    if "$@" >"/exchange/$stage-$arch-$label.log" 2>&1; then
        return 0
    else
        status=$?
        tail -60 "/exchange/$stage-$arch-$label.log" >&2
        return "$status"
    fi
}
expect_failure() {
    local label=$1
    shift
    if run "$label" "$@"; then
        echo "Expected failure: $label" >&2
        exit 1
    fi
}
if [[ $stage == seed ]]; then
    # Start on a real signed ROS snapshot: live ROS packages can be newer
    # than every snapshot, making a live-source lock legitimately degraded.
    fingerprint=4B63CF8FDE49746E98FA01DDAD19BAB3CBF125EA
    curl -fsSL --retry 5 --retry-all-errors --connect-timeout 15 --max-time 60 "https://keyserver.ubuntu.com/pks/lookup?op=get&options=mr&search=0x$fingerprint" -o /tmp/key.asc
    gpg --show-keys --with-colons /tmp/key.asc | grep -q "fpr:::::::::$fingerprint:"
    gpg --dearmor < /tmp/key.asc | sudo tee /usr/share/keyrings/ros2-snapshots-archive-keyring.gpg > /dev/null
    printf 'deb [arch=%s signed-by=/usr/share/keyrings/ros2-snapshots-archive-keyring.gpg] http://snapshots.ros.org/jazzy/%s/ubuntu noble main\n' \
        "$(dpkg --print-architecture)" "$ROS_SNAPSHOT" | sudo tee /etc/apt/sources.list.d/p3-seed.list > /dev/null
    if [[ $arch == aarch64 ]]; then
        cp /exchange/x86-only.lock rospm.lock
        expect_failure missing-platform rospm apply --locked --dry-run
        grep -q 'linux-aarch64' /exchange/seed-aarch64-missing-platform.log
        test ! -d /opt/ros/jazzy
    fi
    run apply rospm apply --mirror official
    run check rospm check --strict
    run repositories rospm check --target "$arch" --strict --json
    run deps rospm deps check
    run lock rospm lock
    python3 /suite/validate.py lock rospm.lock "$arch"
    cp rospm.lock "/exchange/$arch.lock"
    if [[ $arch == x86_64 ]]; then cp rospm.lock /exchange/x86-only.lock; fi
elif [[ $stage == finalize ]]; then
    test "$arch" = x86_64
    cp /exchange/aarch64.lock rospm.lock
    run lock rospm lock
    python3 /suite/validate.py lock rospm.lock both
    cp rospm.lock /exchange/multiplatform.lock
elif [[ $stage == replay ]]; then
    test "$arch" = aarch64
    test ! -d /opt/ros/jazzy
    test ! -d ws/src/p3_native
    cp /exchange/multiplatform.lock rospm.lock
    sha256sum rospm.lock > /tmp/lock.sha256
    run apply-locked rospm apply --locked --mirror official
    sha256sum -c /tmp/lock.sha256
    python3 /suite/validate.py sources rospm.lock
    run check rospm check --strict
    run repositories rospm check --target aarch64 --strict --json
    run verify rospm verify
    run deps rospm deps check
    run build rospm build
    # shellcheck disable=SC1091
    set +u
    source /opt/ros/jazzy/setup.bash
    source ws/install/setup.bash
    set -u
    run ros2-run timeout 180 ros2 run p3_native probe
    grep -q 'P3_NATIVE_OK arch=aarch64 node=p3_native_probe' /exchange/replay-aarch64-ros2-run.log
    readelf -h ws/install/p3_native/lib/p3_native/probe | tee /exchange/replay-aarch64-elf.log | grep -q AArch64
    cp ws/install/p3_native/lib/p3_native/probe /exchange/p3-native-aarch64
    sha256sum /exchange/p3-native-aarch64 > /exchange/replay-aarch64-node.sha256
    test "$(git -C ws/src/p3_native rev-parse HEAD)" = "$(cat /exchange/source.rev)"
    dpkg-query -W -f='${Package}\t${Version}\t${Architecture}\n' 'ros-jazzy-*' binutils-aarch64-linux-gnu > /exchange/replay-aarch64-packages.tsv
    cp rospm.toml /tmp/good-manifest
    sed -i 's/binutils-x86-64-linux-gnu/rospm-p3-deliberately-missing-package/' rospm.toml
    expect_failure missing-package rospm check --target aarch64 --strict --json
    python3 /suite/validate.py missing /exchange/replay-aarch64-missing-package.log
    cp /tmp/good-manifest rospm.toml
    run repeat-apply-locked rospm apply --locked --mirror official
    run repeat-verify rospm verify
    sha256sum -c /tmp/lock.sha256
    run relock rospm lock
    python3 /suite/validate.py compare /exchange/multiplatform.lock rospm.lock
    cp rospm.lock /exchange/replayed.lock
else
    echo "Unknown stage: $stage" >&2
    exit 2
fi
echo "P3_PASS $stage $arch"
