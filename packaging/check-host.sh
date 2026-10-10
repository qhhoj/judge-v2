#!/usr/bin/env bash
# packaging/check-host.sh: verifies a judge host against DEPLOY.md (and the site RUNBOOK "Judge hosts" section).
# Usage: check-host.sh [--bridge HOST:PORT --server-name NAME]
# Env: CHECK_ROOT (default /), CHECK_SKIP_COMMANDS=1 (file checks only), CHECK_KEY_OWNER (default 0),
#      CHECK_FIXTURES (a directory holding saved `isolate-check-environment` and `nft-output` outputs).
set -uo pipefail
R="${CHECK_ROOT:-/}"; R="${R%/}"
DATA=/srv/qhhoj/problem-data
ISOLATE_CONF=/usr/local/etc/isolate
CONFIG=/etc/qhhoj-judge/config.toml
bridge=""; server_name=""
while [ $# -gt 0 ]; do
  case "$1" in
    --bridge) bridge="$2"; shift 2 ;;
    --server-name) server_name="$2"; shift 2 ;;
    *) echo "unknown argument $1" >&2; exit 2 ;;
  esac
done
failed=0
check() { # <description> <command...>
  local what="$1"; shift
  if "$@" >/dev/null 2>&1; then echo "ok    $what"; else echo "FAIL  $what"; failed=1; fi
}
file_is() { [ "$(tr -d '[:space:]' < "$R$1" 2>/dev/null)" = "$2" ]; }
cgroup_v2() { grep -qw memory "$R/sys/fs/cgroup/cgroup.controllers" && grep -qw pids "$R/sys/fs/cgroup/cgroup.controllers"; }
swap_off() { [ "$(wc -l < "$R/proc/swaps")" -le 1 ]; }
smt_off() { grep -qxE 'off|forceoff|notsupported' "$R/sys/devices/system/cpu/smt/control"; }
userns_closed() { file_is /proc/sys/kernel/apparmor_restrict_unprivileged_userns 1 || file_is /proc/sys/kernel/unprivileged_userns_clone 0; }
data_mount_options() { awk -v d="$DATA" '$2 == d && $3 ~ /^nfs/ { print $4 }' "$R/proc/self/mounts"; }
data_mount_safe() {
  local options; options="$(data_mount_options)"; [ -n "$options" ] || return 1
  for flag in ro nosuid nodev noexec; do echo ",$options," | grep -q ",$flag," || return 1; done
  local actimeo; actimeo="$(echo ",$options," | sed -n 's/.*,actimeo=\([0-9]*\),.*/\1/p')"
  [ -n "$actimeo" ] && [ "$actimeo" -le 3 ]
}
key_private() { [ "$(stat -c '%a %u' "$R/etc/qhhoj-judge/key")" = "400 ${CHECK_KEY_OWNER:-0}" ]; }
config_root_644() { [ "$(stat -c '%a %u' "$R$CONFIG")" = "644 ${CHECK_KEY_OWNER:-0}" ]; }
isolate_setuid_root() { stat -c '%a %u' "$R/usr/local/bin/isolate" | grep -qxE "4[0-7]{3} ${CHECK_KEY_OWNER:-0}"; }
user_exists() { grep -q '^qhhoj-judge:' "$R/etc/passwd"; }
# `key = value` from a flat config file (isolate's or the judge's TOML top level); empty when absent.
setting() { sed -n "s/^[[:space:]]*$2[[:space:]]*=[[:space:]]*\"\{0,1\}\([^\"#[:space:]]*\).*/\1/p" "$R$1" | head -n 1; }
# The filesystem type of the longest mount point that contains $1.
fs_type_of() {
  awk -v p="$1" '{ m = $2; if ((p == m || index(p, (m == "/" ? "/" : m "/")) == 1) && length(m) > best) { best = length(m); t = $3 } } END { print t }' "$R/proc/self/mounts"
}
box_root_local() {
  local box_root; box_root="$(setting "$ISOLATE_CONF" box_root)"; [ -n "$box_root" ] || return 1
  local type; type="$(fs_type_of "$box_root")"
  [ -n "$type" ] && ! echo "$type" | grep -qE '^(nfs|cifs|smb|fuse)'
}
online_cpus() { tr ',' '\n' < "$R/sys/devices/system/cpu/online" | awk -F- '{ n += (NF == 2 ? $2 - $1 + 1 : 1) } END { print n }'; }
# The count of a user's first block in /etc/subuid or /etc/subgid; empty when it has none.
subid_count() { awk -F: -v u="$2" '$1 == u { print $3; exit }' "$R$1" 2>/dev/null; }
# isolate's box count: num_boxes when set, else (isolate 2.6 with subid_user) the smaller subuid/subgid block.
isolate_boxes() {
  local num user uids gids
  num="$(setting "$ISOLATE_CONF" num_boxes)"; user="$(setting "$ISOLATE_CONF" subid_user)"
  if [ -n "$num" ] || [ -z "$user" ]; then echo "$num"; return; fi
  uids="$(subid_count /etc/subuid "$user")"; gids="$(subid_count /etc/subgid "$user")"
  [ -n "$uids" ] && [ -n "$gids" ] && echo $(( uids < gids ? uids : gids ))
}
enough_boxes() {
  local num base slots
  num="$(isolate_boxes)"; base="$(setting "$CONFIG" box_base)"; slots="$(setting "$CONFIG" slots)"
  [ -n "$num" ] || return 1
  [ -n "$slots" ] || slots=$(( $(online_cpus) - 1 )); [ "$slots" -ge 1 ] || slots=1
  [ "$num" -ge $(( ${base:-0} + 2 * slots )) ]
}
# Saved output under CHECK_FIXTURES, else the real command.
output_of() { # <fixture name> <command...>
  local name="$1"; shift
  # The command's own status is ignored (isolate-check-environment exits 1 on the waived ASLR check, and pipefail
  # would otherwise turn every caller's `!` into a pass): only its output is parsed.
  if [ -n "${CHECK_FIXTURES:-}" ]; then cat "$CHECK_FIXTURES/$name"; else "$@" 2>&1 || true; fi
}
# isolate's check fails on ASLR=2 by design; that line is the one accepted waiver (RUNBOOK "Judge hosts" step 1).
# Its output is coloured (`ESC[31mFAIL`): the escapes are stripped first, or no word boundary precedes the verdict.
# A missing tool ("command not found") or an empty report has no `Checking for` line and fails too.
isolate_env_clean() {
  local report
  report="$(output_of isolate-check-environment isolate-check-environment | sed 's/\x1b\[[0-9;]*m//g')"
  echo "$report" | grep -q '^Checking for ' || return 1
  ! echo "$report" | grep -iE '\b(FAIL|CAUTION|WARN(ING)?|ERROR)\b' | grep -viqE 'randomi[sz]|ASLR'
}
egress_dropped() { output_of nft-output nft list chain inet filter output | grep -q 'policy drop'; }
isolate_v2() { isolate --version | grep -q 'isolator 2\.'; }
tls_handshake() {
  local host="${bridge%:*}"
  openssl s_client -connect "$bridge" -servername "${server_name:-$host}" -verify_hostname "${server_name:-$host}" \
    -CAfile "$R/etc/qhhoj-judge/bridge-ca.pem" -verify_return_error </dev/null
}

check "cgroups v2 with memory and pids" cgroup_v2
check "swap off" swap_off
check "SMT off (or nosmt)" smt_off
check "kernel.randomize_va_space = 2" file_is /proc/sys/kernel/randomize_va_space 2
check "unprivileged user namespaces restricted" userns_closed
check "$DATA is NFS ro,nosuid,nodev,noexec with actimeo<=3" data_mount_safe
check "judge key is 0400 root" key_private
check "bridge CA present" test -s "$R/etc/qhhoj-judge/bridge-ca.pem"
check "config present" test -s "$R$CONFIG"
check "config is root 644" config_root_644
check "user qhhoj-judge exists" user_exists
check "/usr/local/bin/isolate is setuid root" isolate_setuid_root
check "isolate box_root is on a local filesystem" box_root_local
check "isolate num_boxes >= box_base + 2*slots" enough_boxes
if [ "${CHECK_SKIP_COMMANDS:-0}" != 1 ] || [ -n "${CHECK_FIXTURES:-}" ]; then
  check "isolate-check-environment clean except the ASLR waiver" isolate_env_clean
  check "egress chain inet filter output has policy drop" egress_dropped
fi
if [ "${CHECK_SKIP_COMMANDS:-0}" != 1 ]; then
  check "isolate 2.x" isolate_v2
  check "isolate.service active" systemctl is-active --quiet isolate.service
  [ -n "$bridge" ] && check "TLS to $bridge verifies (CA + name)" tls_handshake
fi
exit "$failed"
