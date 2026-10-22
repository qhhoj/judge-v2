#!/bin/sh
# Container entrypoint: gives isolate a cgroup v2 subtree it may write, then runs the command (the judge by default).
# Replaces isolate-cg-keeper, which needs the cpuset controller; a container usually has only memory and pids.
# cpuset is enabled when the container has it, so isolate's box<id>.cpus lines can pin boxes (DEPLOY.md).
set -e
cg=/sys/fs/cgroup
mkdir -p "$cg/daemon"
echo $$ > "$cg/daemon/cgroup.procs"
echo "+memory +pids" > "$cg/cgroup.subtree_control"
if grep -qw cpuset "$cg/cgroup.controllers" && ! echo "+cpuset" > "$cg/cgroup.subtree_control"; then
  echo "cg-setup: cpuset refused; boxes stay pinned by taskset only" >&2
fi
mkdir -p /run/isolate/locks
echo "$cg" > /run/isolate/cgroup
exec "$@"
