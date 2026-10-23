#!/usr/bin/env bash
# only-stdio.sh CMD...: runs CMD with no inheritable fd above 2. The GitHub runner passes extra fds to each step;
# channel runs (isolate --inherit-fds) refuse to start while the process holds one, as the judge's systemd unit
# guarantees in production. Close-on-exec fds are left alone: they never reach CMD.
set -eu
for fd in $(ls /proc/$$/fd); do
  [ "$fd" -gt 2 ] || continue
  [[ -e /proc/$$/fd/$fd ]] || continue # gone since the listing: the command substitution's own pipe
  flags=$(sed -n 's/^flags:[[:space:]]*//p' "/proc/$$/fdinfo/$fd" 2>/dev/null || true)
  [ -n "$flags" ] || continue
  if (( (8#$flags & 8#2000000) == 0 )); then
    echo "only-stdio: closing inherited fd $fd ($(readlink "/proc/$$/fd/$fd" || true))" >&2
    eval "exec $fd>&-"
  fi
done
exec "$@"
