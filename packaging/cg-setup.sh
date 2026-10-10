#!/bin/sh
# Container entrypoint: gives isolate a cgroup v2 subtree it may write, then runs the command (the judge by default).
# Replaces isolate-cg-keeper, which needs the cpuset controller; a container usually has only memory and pids.
set -e
cg=/sys/fs/cgroup
mkdir -p "$cg/daemon"
echo $$ > "$cg/daemon/cgroup.procs"
echo "+memory +pids" > "$cg/cgroup.subtree_control"
mkdir -p /run/isolate/locks
echo "$cg" > /run/isolate/cgroup
exec "$@"
