#!/usr/bin/env bash
# Validate the process/session assumptions required by the first 4.5.2 item.
# This is a prototype, not the production worker or socket protocol.
set -euo pipefail

mode=${1:-run}
dir=${2:-}

case "$mode" in
  child)
    dir=$2
    printf '%s\n' "$$" >"$dir/child.pid"
    printf 'uid=%s\npgid=%s\nsid=%s\nstdin=%s\nstdout=%s\nstderr=%s\n' \
      "$(id -u)" \
      "$(ps -o pgid= -p $$ | tr -d ' ')" \
      "$(ps -o sid= -p $$ | tr -d ' ')" \
      "$(readlink /proc/$$/fd/0)" \
      "$(readlink /proc/$$/fd/1)" \
      "$(readlink /proc/$$/fd/2)" >"$dir/child.fds"
    for n in 1 2 3 4 5; do
      printf 'tick %s\n' "$n" >>"$dir/child.log"
      sleep 0.2
    done
    : >"$dir/child.done"
    ;;
  worker)
    dir=$2
    # A worker must not die when its controlling terminal/session disappears.
    trap '' HUP
    printf '%s\n' "$$" >"$dir/worker.pid"
    printf 'uid=%s\npgid=%s\nsid=%s\nstdin=%s\nstdout=%s\nstderr=%s\n' \
      "$(id -u)" \
      "$(ps -o pgid= -p $$ | tr -d ' ')" \
      "$(ps -o sid= -p $$ | tr -d ' ')" \
      "$(readlink /proc/$$/fd/0)" \
      "$(readlink /proc/$$/fd/1)" \
      "$(readlink /proc/$$/fd/2)" >"$dir/worker.fds"
    setsid "$0" child "$dir" </dev/null >"$dir/child.log" 2>&1 &
    child=$!
    printf '%s\n' "$child" >"$dir/child.launcher.pid"
    wait "$child"
    : >"$dir/worker.done"
    ;;
  controller)
    dir=$2
    # Closing all three descriptors models a detached SSH/TUI launcher.
    printf 'pgid=%s\nsid=%s\n' \
      "$(ps -o pgid= -p $$ | tr -d ' ')" \
      "$(ps -o sid= -p $$ | tr -d ' ')" >"$dir/controller.session"
    setsid "$0" worker "$dir" </dev/null >"$dir/worker.log" 2>&1 &
    printf '%s\n' "$!" >"$dir/controller.worker.pid"
    : >"$dir/controller.ready"
    # Keep the controller alive long enough for the harness to kill it.
    sleep 30
    ;;
  run)
    dir=$(mktemp -d "${TMPDIR:-/tmp}/rospm-p45-worker.XXXXXX")
    cleanup() {
      for file in worker.pid child.pid; do
        if [[ -s "$dir/$file" ]]; then
          kill -KILL "$(cat "$dir/$file")" 2>/dev/null || true
        fi
      done
      rm -rf "$dir"
    }
    trap cleanup EXIT

    # The controller is the stand-in for a TUI process. It launches the worker
    # in a new session and then is killed before the task finishes.
    setsid "$0" controller "$dir" </dev/null >"$dir/controller.log" 2>&1 &
    controller=$!
    for _ in {1..50}; do
      [[ -s "$dir/controller.worker.pid" && -e "$dir/worker.fds" ]] && break
      sleep 0.02
    done
    [[ -s "$dir/controller.worker.pid" && -e "$dir/worker.fds" ]]
    worker=$(cat "$dir/controller.worker.pid")
    kill -KILL "$controller"
    wait "$controller" 2>/dev/null || true

    # A SIGHUP must not terminate the detached worker either.
    kill -HUP "$worker" 2>/dev/null || true
    for _ in {1..100}; do
      [[ -e "$dir/child.done" && -e "$dir/worker.done" ]] && break
      sleep 0.05
    done
    [[ -e "$dir/child.done" && -e "$dir/worker.done" ]]

    worker_uid=$(sed -n 's/^uid=//p' "$dir/worker.fds")
    child_uid=$(sed -n 's/^uid=//p' "$dir/child.fds")
    worker_pgid=$(sed -n 's/^pgid=//p' "$dir/worker.fds")
    child_pgid=$(sed -n 's/^pgid=//p' "$dir/child.fds")
    controller_pgid=$(sed -n 's/^pgid=//p' "$dir/controller.session")
    worker_sid=$(sed -n 's/^sid=//p' "$dir/worker.fds")
    child_sid=$(sed -n 's/^sid=//p' "$dir/child.fds")
    [[ "$worker_uid" == "$(id -u)" && "$child_uid" == "$(id -u)" ]]
    [[ "$worker_pgid" != "$controller_pgid" && "$worker_pgid" != "$child_pgid" ]]
    [[ "$worker_sid" != "$child_sid" ]]
    grep -qx 'stdin=/dev/null' "$dir/worker.fds"
    grep -qx 'stdin=/dev/null' "$dir/child.fds"

    printf 'worker prototype passed\n'
    printf 'worker uid=%s pgid=%s sid=%s; child uid=%s pgid=%s sid=%s\n' \
      "$worker_uid" "$worker_pgid" "$worker_sid" "$child_uid" "$child_pgid" "$child_sid"
    printf 'controller killed; SIGHUP sent; child and worker completed\n'
    ;;
  *)
    printf 'usage: %s [run]\n' "$0" >&2
    exit 2
    ;;
esac
