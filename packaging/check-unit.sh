#!/usr/bin/env bash
# packaging/check-unit.sh: the unit keeps the directives the spec and runbook rely on.
set -euo pipefail
unit="$(dirname "$0")/qhhoj-judge.service"
need() { grep -qxF "$1" "$unit" || { echo "missing: $1" >&2; exit 1; }; }
refuse() { if grep -q "^$1" "$unit"; then echo "must not set: $1" >&2; exit 1; fi; }
need "Requires=isolate.service"
need "LoadCredential=judge-key:/etc/qhhoj-judge/key"
need "Restart=always"
need "TimeoutStopSec=150"
need "ProtectSystem=strict"
need "ReadOnlyPaths=/srv/qhhoj/problem-data"
need "ReadWritePaths=/var/cache/qhhoj-judge /var/local/lib/isolate /run/isolate"
need "UMask=0022"
need "CacheDirectoryMode=0700"
need "RestrictSUIDSGID=yes"
need "ProtectKernelLogs=yes"
need "ProtectHostname=yes"
need "LockPersonality=yes"
# Each of these breaks setuid isolate, its cgroups or namespaces, or the JVM/PyPy JITs (preflight I12).
for setting in NoNewPrivileges=yes NoNewPrivileges=true SystemCallFilter SystemCallArchitectures MemoryDenyWriteExecute \
               ProtectKernelTunables ProtectControlGroups PrivateDevices PrivateNetwork RestrictNamespaces \
               CapabilityBoundingSet PrivateUsers DynamicUser; do refuse "$setting"; done
if command -v systemd-analyze >/dev/null; then
  # Only messages about this unit count: the host's other units may print warnings of their own.
  # The binary and the dependency units (isolate.service) do not exist on a build host, so "not executable"
  # and "Unit ... not found" are tolerated.
  verify_output="$(systemd-analyze verify "$unit" 2>&1 || true)"
  unexpected="$(grep -F "qhhoj-judge.service" <<<"$verify_output" \
    | grep -v "Command .* is not executable\|is not an absolute path\|Failed to create qhhoj-judge.service/start: Unit .* not found" || true)"
  if [[ -n "$unexpected" ]]; then
    echo "$unexpected" >&2
    exit 1
  fi
fi
echo "unit ok"
