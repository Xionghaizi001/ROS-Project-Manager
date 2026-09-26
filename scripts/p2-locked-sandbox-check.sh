#!/usr/bin/env bash
# P2 `apply --locked` 非破坏性核验（docs/p2-locked-validation.md）。
#
# 不修改系统：不写 /etc/apt，不装包，不用 sudo。做的事：
#   1. 用本地 git 上游建临时项目，真实联网执行 rospm check / doctor / lock / verify / deps check；
#   2. 在隔离的 apt 目录（APT_CONFIG）里配置锁文件记录的 ROS / Ubuntu 快照，逐条核对
#      锁定版本在快照中是否可下载，并对比实时 ROS 源的底包版本；
#   3. 按"快照日装好 ROS 的机器"合成一份完整锁文件，生成 apply --locked --dry-run 计划，
#      用 apt-get -s 在空 dpkg 状态（干净机器）与本机 dpkg 状态（+ 本机实时 Ubuntu 源）下模拟
#      计划中的锁定安装命令。
#
# 用法：scripts/p2-locked-sandbox-check.sh [WORKDIR]
#   ROSPM=<rospm 可执行文件>（默认 target/release/rospm）
#   发行版与 Ubuntu 版本从 /etc/os-release 推导：22.04 → humble，24.04 → jazzy。
# 结果：WORKDIR/log/ 下的编号日志，最后打印一张汇总表（同时写入 WORKDIR/log/summary.txt）。
set -uo pipefail

REPO=$(cd "$(dirname "$0")/.." && pwd)
ROSPM=${ROSPM:-$REPO/target/release/rospm}
WORK=${1:-/tmp/rospm-p2-check-$(date -u +%Y%m%dT%H%M%SZ)}
[ -x "$ROSPM" ] || { echo "rospm not found at $ROSPM (cargo build --release)"; exit 2; }

. /etc/os-release
case "$VERSION_ID" in
  22.04) DISTRO=humble ;;
  24.04) DISTRO=jazzy ;;
  *) echo "unsupported Ubuntu $VERSION_ID"; exit 2 ;;
esac
CODENAME=$VERSION_CODENAME
ARCH=$(dpkg --print-architecture)
LOG=$WORK/log
SB=$WORK/apt-sandbox
mkdir -p "$LOG" "$SB"
SUMMARY=$LOG/summary.txt
: >"$SUMMARY"
sum() { printf '%-44s %s\n' "$1" "$2" | tee -a "$SUMMARY"; }
run() { # run <logname> <cmd...>: log stdout+stderr and the exit code
  local name=$1; shift
  { date -u +%FT%TZ; "$@"; echo "exit $?"; } >"$LOG/$name.txt" 2>&1
  tail -1 "$LOG/$name.txt" | awk '{print $2}'
}

echo "work dir: $WORK  (Ubuntu $VERSION_ID / $DISTRO / $ARCH)"
sum "time (UTC)" "$(date -u +%FT%TZ)"
sum "host" "Ubuntu $VERSION_ID $CODENAME $ARCH, rospm $("$ROSPM" --version | awk '{print $2}') @ $(git -C "$REPO" describe --always --dirty)"
sum "ROS installed (/opt/ros)" "$(ls /opt/ros 2>/dev/null | tr '\n' ' ' || true)"

# --- 1. scratch project with a local git upstream (no GitHub needed) ---------
P=$WORK/proj
mkdir -p "$P/upstream/demo_pkg"
(
  cd "$P/upstream/demo_pkg" && git init -q -b main
  cat >package.xml <<'EOF'
<?xml version="1.0"?>
<package format="3">
  <name>demo_pkg</name>
  <version>0.0.1</version>
  <description>rospm P2 validation package</description>
  <maintainer email="dev@example.com">dev</maintainer>
  <license>Apache-2.0</license>
  <buildtool_depend>ament_cmake</buildtool_depend>
  <depend>rclcpp</depend>
  <depend>std_msgs</depend>
  <build_depend>libopencv-dev</build_depend>
  <export><build_type>ament_cmake</build_type></export>
</package>
EOF
  git add . && git -c user.name=t -c user.email=t@t commit -qm init
)
cat >"$P/rospm.toml" <<EOF
schema_version = 1

[project]
name = "p2check"
ros_distro = "$DISTRO"
platforms = ["linux-$(uname -m)"]

[environment]
provider = "native"

[environment.native]
ubuntu = "$VERSION_ID"
ros_base = "ros-$DISTRO-ros-base"

[workspaces.main]
path = "ws"
sources = "main.repos"
underlays = []

[dependencies]
apt = ["curl"]
pip = ["numpy"]

[dependencies.rosdep]
enabled = true
EOF
cat >"$P/main.repos" <<EOF
repositories:
  demo_pkg:
    type: git
    url: file://$P/upstream/demo_pkg
    version: main
EOF
mkdir -p "$P/ws/src" && git clone -q "file://$P/upstream/demo_pkg" "$P/ws/src/demo_pkg"

cd "$P" || exit 2
sum "rospm check" "exit $(run 01-check "$ROSPM" check)"
sum "rospm doctor" "exit $(run 02-doctor "$ROSPM" doctor)"
sum "rospm apply --dry-run" "exit $(run 03-apply-dry-run "$ROSPM" apply --dry-run)"
sum "rospm lock (network)" "exit $(run 04-lock "$ROSPM" lock)"
ROS_SNAP=$(sed -n 's/^ros_snapshot = "\(.*\)"/\1/p' rospm.lock)
UBU_SNAP=$(sed -n 's/^ubuntu_snapshot = "\(.*\)"/\1/p' rospm.lock)
sum "  ROS snapshot / Ubuntu snapshot" "${ROS_SNAP:-<none>} / ${UBU_SNAP:-<none>}"
sum "rospm verify" "exit $(run 05-verify "$ROSPM" verify)"
sum "rospm deps check" "exit $(run 06-deps-check "$ROSPM" deps check)"
sum "rospm apply --locked --dry-run (this lock)" "exit $(run 07-apply-locked-dry-run "$ROSPM" apply --locked --dry-run)"

if [ -z "$ROS_SNAP" ] || [ -z "$UBU_SNAP" ]; then
  sum "sandbox" "skipped: lock has no snapshot (see log/04-lock.txt)"
  exit 1
fi

# --- 2. isolated apt pointing at the locked snapshots -------------------------
mkdir -p "$SB"/{lists/partial,cache/archives/partial,parts,prefs,keys}
curl -sSf --max-time 60 -o "$SB/keys/ros-snap.asc" \
  'https://keyserver.ubuntu.com/pks/lookup?op=get&search=0x4B63CF8FDE49746E98FA01DDAD19BAB3CBF125EA'
FPR=$(gpg --show-keys --with-colons "$SB/keys/ros-snap.asc" | awk -F: '$1=="fpr"{print $10; exit}')
[ "$FPR" = 4B63CF8FDE49746E98FA01DDAD19BAB3CBF125EA ] || { sum "snapshot key" "fingerprint mismatch: $FPR"; exit 1; }
gpg --dearmor <"$SB/keys/ros-snap.asc" >"$SB/keys/ros-snap.gpg"
UK=/usr/share/keyrings/ubuntu-archive-keyring.gpg
{
  echo "deb [arch=$ARCH signed-by=$SB/keys/ros-snap.gpg] http://snapshots.ros.org/$DISTRO/$ROS_SNAP/ubuntu $CODENAME main"
  for s in "$CODENAME" "$CODENAME-updates" "$CODENAME-security"; do
    echo "deb [arch=$ARCH signed-by=$UK] https://snapshot.ubuntu.com/ubuntu/$UBU_SNAP $s main restricted universe multiverse"
  done
} >"$SB/snapshot.list"
: >"$SB/empty-status"
mkconf() { # mkconf <name> <sources.list> <dpkg status>
  mkdir -p "$SB/$1/lists/partial" "$SB/$1/cache/archives/partial"
  cat >"$SB/$1/apt.conf" <<EOF
Dir::Etc::SourceList "$2";
Dir::Etc::SourceParts "$SB/parts";
Dir::Etc::PreferencesParts "$SB/prefs";
Dir::State::Lists "$SB/$1/lists";
Dir::Cache "$SB/$1/cache";
Dir::Cache::pkgcache "";
Dir::Cache::srcpkgcache "";
Dir::State::status "$3";
Debug::NoLocking "true";
APT::Sandbox::User "$(id -un)";
EOF
}
mkconf clean "$SB/snapshot.list" "$SB/empty-status"
# host: snapshots + the host's own live Ubuntu sources (what a real apply --locked sees)
{ cat "$SB/snapshot.list"; grep -hvE '^\s*(#|$)' /etc/apt/sources.list 2>/dev/null; } >"$SB/host.list"
mkconf host "$SB/host.list" /var/lib/dpkg/status
if ls /etc/apt/sources.list.d/*.sources >/dev/null 2>&1; then
  # deb822 (24.04 ubuntu.sources): include Ubuntu's own file, never ROS ones
  for f in /etc/apt/sources.list.d/ubuntu.sources; do [ -f "$f" ] && cp "$f" "$SB/parts/"; done
  sed -i "s#^Dir::Etc::SourceParts .*#Dir::Etc::SourceParts \"$SB/parts\";#" "$SB/host/apt.conf"
  # the clean config must not see Ubuntu's live file
  mkdir -p "$SB/noparts" && sed -i "s#^Dir::Etc::SourceParts .*#Dir::Etc::SourceParts \"$SB/noparts\";#" "$SB/clean/apt.conf"
fi
aptc() { APT_CONFIG=$SB/clean/apt.conf LANG=C "$@"; }
apth() { APT_CONFIG=$SB/host/apt.conf LANG=C "$@"; }
sum "sandbox apt-get update (snapshots)" "exit $(run 08-sandbox-update aptc apt-get update)"
sum "sandbox apt-get update (host+snapshots)" "exit $(run 09-sandbox-host-update apth apt-get update)"
grep -hE '^(W|E):' "$LOG/08-sandbox-update.txt" "$LOG/09-sandbox-host-update.txt" | head -5 | sed 's/^/  /' | tee -a "$SUMMARY"

# --- 3. each locked version downloadable from the snapshot? --------------------
lock_entries() { awk -F' = ' -v plat="linux-$(uname -m)" '
  /^\[\[apt\]\]/{inapt=1; next} /^\[/{inapt=0}
  inapt && /^platform = /{gsub(/"/,"",$2); p=$2}
  inapt && /^name = /{gsub(/"/,"",$2); n=$2}
  inapt && /^version = /{gsub(/"/,"",$2); if(p==plat) print n, $2}' "$1"; }
missing=0 total=0
while read -r n v; do
  total=$((total + 1))
  have=$(aptc apt-cache madison "$n" 2>/dev/null | awk -F'|' '{gsub(/ /,"",$2); print $2}' | sort -u | tr '\n' ' ')
  if echo " $have" | grep -qF " $v "; then echo "IN-SNAPSHOT      $n=$v"
  else missing=$((missing + 1)); echo "NOT-IN-SNAPSHOT  $n=$v  (snapshot has: $have)"; fi
done < <(lock_entries rospm.lock) >"$LOG/10-lock-availability.txt"
sum "locked versions not in snapshot (this lock)" "$missing / $total"
grep NOT-IN "$LOG/10-lock-availability.txt" | sed 's/^/  /' | tee -a "$SUMMARY"

# live ROS repo vs snapshot for the base package
LIVE=$(curl -sS --max-time 120 "http://packages.ros.org/ros2/ubuntu/dists/$CODENAME/main/binary-$ARCH/Packages.gz" | zcat 2>/dev/null |
  awk -v p="ros-$DISTRO-ros-base" '/^Package: /{x=($2==p)} x&&/^Version: /{print $2}')
SNAPV=$(aptc apt-cache madison "ros-$DISTRO-ros-base" | awk -F'|' '{gsub(/ /,"",$2); print $2; exit}')
sum "ros-$DISTRO-ros-base live / snapshot" "${LIVE:-?} / ${SNAPV:-?}"
curl -sS --max-time 60 "http://snapshots.ros.org/$DISTRO/" | grep -oE '20[0-9]{2}-[0-9]{2}-[0-9]{2}' | sort -u | tail -3 >"$LOG/11-snapshot-dates.txt"
sum "newest ROS snapshot dates" "$(tr '\n' ' ' <"$LOG/11-snapshot-dates.txt")"

# --- 4. synthetic lock = a machine that installed from this snapshot ----------
aptc apt-get -s install -y "ros-$DISTRO-ros-base" python3-rosdep python3-vcstool python3-colcon-common-extensions curl \
  >"$LOG/12-sim-clean-install.txt" 2>&1
sum "sim: clean install from snapshot" "exit $?, $(grep -c '^Inst ' "$LOG/12-sim-clean-install.txt") pkgs"
Y=$WORK/synth
mkdir -p "$Y/ws/src" && cp rospm.toml main.repos "$Y/" && git clone -q "file://$P/upstream/demo_pkg" "$Y/ws/src/demo_pkg"
awk '/^\[\[apt\]\]/{exit} {print}' rospm.lock >"$Y/rospm.lock"
grep '^Inst ' "$LOG/12-sim-clean-install.txt" | sed -E 's/^Inst ([^ ]+) (\[[^]]*\] )?\(([^ ]+) .*/\1 \3/' |
  awk -v d="ros-$DISTRO-" 'index($1,d)==1 || $1=="python3-rosdep" || $1=="python3-vcstool" || $1=="python3-colcon-common-extensions" || $1=="curl"' |
  sort | while read -r n v; do printf '[[apt]]\nplatform = "linux-%s"\nname = "%s"\nversion = "%s"\n\n' "$(uname -m)" "$n" "$v"; done >>"$Y/rospm.lock"
cd "$Y" || exit 2
sum "apply --locked --dry-run (synthetic lock)" "exit $(run 13-apply-locked-dry-run-synth "$ROSPM" apply --locked --dry-run)"
PL=$LOG/13-apply-locked-dry-run-synth.txt
w=$(grep -n 'rospm-ubuntu-snapshot.list' "$PL" | grep -v '#' | head -1 | cut -d: -f1)
i=$(grep -n 'allow-downgrades' "$PL" | head -1 | cut -d: -f1)
if [ -n "$w" ] && [ -n "$i" ] && sed -n "${w},${i}p" "$PL" | grep -q 'apt-get update'; then
  sum "  update between snapshot write and install" "yes"
else sum "  update between snapshot write and install" "NO (or step absent)"; fi
grep -E 'allow-downgrades' "$PL" | sed -E 's/^\[[0-9]+\] \$ sudo apt-get install -y --allow-downgrades //' >"$SB/locked-cmds.txt"
ALL=$(tr '\n' ' ' <"$SB/locked-cmds.txt")
aptc apt-get -s install -y --allow-downgrades $ALL >"$LOG/14-sim-locked-clean.txt" 2>&1
sum "sim: locked install, clean machine" "exit $?, $(grep -c '^Inst ' "$LOG/14-sim-locked-clean.txt") pkgs"
apth apt-get -s install -y --allow-downgrades $ALL >"$LOG/15-sim-locked-host.txt" 2>&1
rc=$?
sum "sim: locked install, this host (+live Ubuntu)" "exit $rc, $(grep -c '^Inst ' "$LOG/15-sim-locked-host.txt") pkgs"
[ $rc -ne 0 ] && grep -E '^ [^ ].*Depends:' "$LOG/15-sim-locked-host.txt" | head -5 | sed 's/^/  /' | tee -a "$SUMMARY"
echo "logs: $LOG"
