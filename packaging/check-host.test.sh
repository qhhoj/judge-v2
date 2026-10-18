#!/usr/bin/env bash
# packaging/check-host.test.sh: check-host.sh on a fake filesystem; one good host, then one broken setting at a time.
set -euo pipefail
here="$(cd "$(dirname "$0")" && pwd)"
root="$(mktemp -d)"; fixtures="$(mktemp -d)"; trap 'rm -rf "$root" "$fixtures"' EXIT

good_host() {
  rm -rf "${root:?}"/* "${fixtures:?}"/*
  mkdir -p "$root/sys/fs/cgroup" "$root/sys/devices/system/cpu/smt" "$root/proc/sys/kernel" "$root/etc/qhhoj-judge" "$root/proc/self" \
    "$root/usr/local/bin" "$root/usr/local/etc" "$root/run/isolate" "$root/sys/fs/cgroup/isolate.slice/isolate.service"
  echo "cpuset cpu io memory pids" > "$root/sys/fs/cgroup/cgroup.controllers"
  printf 'Filename\tType\tSize\tUsed\tPriority\n' > "$root/proc/swaps"
  echo off > "$root/sys/devices/system/cpu/smt/control"
  echo 0-7 > "$root/sys/devices/system/cpu/online"
  echo 2 > "$root/proc/sys/kernel/randomize_va_space"
  echo 1 > "$root/proc/sys/kernel/apparmor_restrict_unprivileged_userns"
  {
    echo "/dev/sda1 / ext4 rw,relatime 0 0"
    echo "site:/srv/qhhoj/problem-data /srv/qhhoj/problem-data nfs4 ro,nosuid,nodev,noexec,relatime,vers=4.2,actimeo=1,hard 0 0"
  } > "$root/proc/self/mounts"
  echo key > "$root/etc/qhhoj-judge/key"; chmod 0400 "$root/etc/qhhoj-judge/key"
  echo pem > "$root/etc/qhhoj-judge/bridge-ca.pem"
  printf "name = 'j'\nbox_base = 0\n" > "$root/etc/qhhoj-judge/config.toml"; chmod 0644 "$root/etc/qhhoj-judge/config.toml"
  echo "qhhoj-judge:x:990:990::/nonexistent:/usr/sbin/nologin" > "$root/etc/passwd"
  echo binary > "$root/usr/local/bin/isolate"; chmod 4755 "$root/usr/local/bin/isolate"
  printf 'box_root = /var/local/lib/isolate\nnum_boxes = 1000\ncg_root = auto:/run/isolate/cgroup\n' > "$root/usr/local/etc/isolate"
  # isolate's cg keeper writes its cgroup to /run/isolate/cgroup; memory.peak exists from kernel 5.19
  echo /sys/fs/cgroup/isolate.slice/isolate.service > "$root/run/isolate/cgroup"
  echo 0 > "$root/sys/fs/cgroup/isolate.slice/isolate.service/memory.peak"
  # isolate 2.6's real wording and colours on a tuned host: only the ASLR check fails.
  printf 'Checking for cgroup support for memory.max ... \e[32mPASS\e[m\nChecking for kernel address space randomisation ... \e[31mFAIL\e[m\n\e[33mWARNING:\e[m address space randomisation is enabled.\necho 0 > /proc/sys/kernel/randomize_va_space\n' > "$fixtures/isolate-check-environment"
  printf 'table inet filter {\n\tchain output {\n\t\ttype filter hook output priority filter; policy drop;\n\t}\n}\n' > "$fixtures/nft-output"
}

# isolate 2.6's shipped config: the box count comes from the subid_user's /etc/subuid and /etc/subgid blocks.
subid_host() { # <subuid count> <subgid count>
  sed -i "s/num_boxes = 1000/subid_user = isolate/" "$root/usr/local/etc/isolate"
  echo "isolate:200000:$1" > "$root/etc/subuid"; echo "isolate:200000:$2" > "$root/etc/subgid"
}

run() { CHECK_ROOT="$root" CHECK_SKIP_COMMANDS=1 CHECK_FIXTURES="$fixtures" CHECK_KEY_OWNER="$(id -u)" bash "$here/check-host.sh"; }

expect_fail() { # <what> <command that breaks the host>
  good_host; eval "$2"
  if run >/dev/null 2>&1; then echo "not caught: $1" >&2; exit 1; fi
}

expect_pass() { # <what> <command that changes the host>
  good_host; eval "$2"
  run >/dev/null || { echo "must pass: $1" >&2; run; exit 1; }
}

good_host
run >/dev/null || { echo "a good host must pass" >&2; run; exit 1; }
expect_fail "cgroups v1" 'rm "$root/sys/fs/cgroup/cgroup.controllers"'
expect_fail "swap on" 'printf "/swap.img file 1 0 -2\n" >> "$root/proc/swaps"'
expect_fail "SMT on" 'echo on > "$root/sys/devices/system/cpu/smt/control"'
expect_fail "ASLR weak" 'echo 1 > "$root/proc/sys/kernel/randomize_va_space"'
expect_fail "userns open" 'echo 0 > "$root/proc/sys/kernel/apparmor_restrict_unprivileged_userns"'
expect_fail "rw mount" 'sed -i "s/ ro,/ rw,/" "$root/proc/self/mounts"'
expect_fail "exec mount" 'sed -i "s/noexec,//" "$root/proc/self/mounts"'
expect_fail "slow attribute cache" 'sed -i "s/actimeo=1/actimeo=60/" "$root/proc/self/mounts"'
expect_fail "no data mount" 'echo > "$root/proc/self/mounts"'
expect_fail "key readable" 'chmod 0644 "$root/etc/qhhoj-judge/key"'
expect_fail "no CA" 'rm "$root/etc/qhhoj-judge/bridge-ca.pem"'
expect_fail "no user" 'echo > "$root/etc/passwd"'
expect_fail "config writable" 'chmod 0664 "$root/etc/qhhoj-judge/config.toml"'
expect_fail "isolate not setuid" 'chmod 0755 "$root/usr/local/bin/isolate"'
expect_fail "no memory.peak (kernel before 5.19)" 'rm "$root/sys/fs/cgroup/isolate.slice/isolate.service/memory.peak"'
expect_fail "no cgroup path from isolate's cg keeper" 'rm "$root/run/isolate/cgroup"'
expect_fail "box_root on NFS" 'echo "site:/boxes /var/local/lib/isolate nfs4 rw 0 0" >> "$root/proc/self/mounts"'
expect_fail "too few boxes for 7 default slots" 'sed -i "s/num_boxes = 1000/num_boxes = 13/" "$root/usr/local/etc/isolate"'
expect_fail "too few boxes for box_base" 'sed -i "s/box_base = 0/box_base = 990/" "$root/etc/qhhoj-judge/config.toml"'
expect_fail "too few subgids for 7 default slots" 'subid_host 65536 13'
expect_fail "subid_user without a subuid block" 'subid_host 65536 65536; rm "$root/etc/subuid"'
expect_fail "isolate env problem besides ASLR" 'echo "WARNING: transparent hugepages are enabled" >> "$fixtures/isolate-check-environment"'
expect_fail "coloured FAIL besides ASLR" 'printf "Checking for simultaneous multithreading ... \\e[31mFAIL\\e[m\n" >> "$fixtures/isolate-check-environment"'
expect_fail "coloured CAUTION (swap)" 'printf "Checking for swap ... \\e[33mCAUTION\\e[m\n" >> "$fixtures/isolate-check-environment"'
expect_fail "egress policy accept" 'sed -i "s/policy drop/policy accept/" "$fixtures/nft-output"'
# The real commands (stubs on PATH): isolate-check-environment exits 1 on the waived ASLR check, as on a real host.
stubs="$(mktemp -d)"; trap 'rm -rf "$root" "$fixtures" "$stubs"' EXIT
printf '#!/bin/sh\ncat "%s/isolate-check-environment"; exit 1\n' "$fixtures" > "$stubs/isolate-check-environment"
printf '#!/bin/sh\ncat "%s/nft-output"\n' "$fixtures" > "$stubs/nft"
printf '#!/bin/sh\necho "The process isolator 2.6"\n' > "$stubs/isolate"
printf '#!/bin/sh\nexit 0\n' > "$stubs/systemctl"
chmod +x "$stubs"/*
run_commands() { PATH="$stubs:$PATH" CHECK_ROOT="$root" CHECK_KEY_OWNER="$(id -u)" bash "$here/check-host.sh"; }
good_host
run_commands >/dev/null || { echo "a good host must pass with its real commands" >&2; run_commands; exit 1; }
good_host; echo "WARNING: transparent hugepages are enabled" >> "$fixtures/isolate-check-environment"
if run_commands >/dev/null 2>&1; then echo "not caught: THP warning from a failing isolate-check-environment" >&2; exit 1; fi
good_host; : > "$fixtures/isolate-check-environment"
if run >/dev/null 2>&1; then echo "not caught: empty isolate-check-environment report" >&2; exit 1; fi
# isolate-check-environment not installed (or not on PATH): FAIL, never clean.
missing="$(mktemp -d)"; trap 'rm -rf "$root" "$fixtures" "$stubs" "$missing"' EXIT
cp "$stubs/nft" "$stubs/isolate" "$stubs/systemctl" "$missing/"
good_host
if PATH="$missing:/usr/bin:/bin" CHECK_ROOT="$root" CHECK_KEY_OWNER="$(id -u)" bash "$here/check-host.sh" 2>&1 \
  | grep -q '^ok    isolate-check-environment'; then echo "not caught: isolate-check-environment missing" >&2; exit 1; fi

# Exactly enough boxes passes: 7 default slots (8 CPUs - 1) need 14 boxes from 0.
expect_pass "num_boxes = box_base + 2*slots" 'sed -i "s/num_boxes = 1000/num_boxes = 14/" "$root/usr/local/etc/isolate"'
# An explicit slots value wins over the CPU count.
expect_pass "slots = 2 needs only 4 boxes" 'printf "slots = 2\n" >> "$root/etc/qhhoj-judge/config.toml"; sed -i "s/num_boxes = 1000/num_boxes = 4/" "$root/usr/local/etc/isolate"'
expect_pass "subid_user with 14 subuids and subgids" 'subid_host 14 14'
expect_pass "a fixed cg_root with memory.peak" \
  'sed -i "s|auto:/run/isolate/cgroup|/sys/fs/cgroup/isolate.slice/isolate.service|" "$root/usr/local/etc/isolate"; rm "$root/run/isolate/cgroup"'
echo "check-host tests ok"
