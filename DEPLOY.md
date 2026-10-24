# Deploying a judge-v2 host

This guide sets up one judge machine for QHHOJ. The judge (`qhhoj-judge`) runs submissions in
[isolate](https://github.com/ioi/isolate) and connects out to the site's judge bridge. Nothing connects in to
the judge except your SSH.

- **You** (the judge deployer) own the judge machine: everything in Phase A and Phase B below.
- **The site admin** owns the site host (qhhoj.com; the site admin gives you its public IP): the tunnel endpoint, the problem-data
  export, the bridge, and the judge's name and key. Their side is listed in the appendix so you know what to
  expect from them.

Read the whole guide once before starting. Every command runs as `root` on the judge machine unless it says
otherwise.

## What you need

- A **dedicated** machine: Ubuntu 24.04 LTS, x86-64, at least 4 physical cores and 8 GB RAM, 40 GB free
  local disk. Nothing else may run on it: the judge tunes the whole kernel and drops all other outgoing
  traffic. A VM is fine if its vCPUs are not overcommitted.
- Root SSH access to it, and the address range you SSH from (your "admin network").
- Read access to the GitHub repository `qhhoj/judge-v2` (ask the site admin), or the release tarball sent
  to you by the site admin.

## Exchange with the site admin

| You send | When | They send back |
| --- | --- | --- |
| The judge machine's public IPv4 address | Before Phase A step 5 | The site's WireGuard public key and port (51820/udp) |
| The judge's WireGuard **public** key (step 5) | Phase A step 5 | Confirmation that the tunnel and the NFS export are open |
| — | Before Phase A step 7 | `bridge-ca.pem` (a public certificate, safe to send by email) |
| "Phase A done", with the output of `selftest` and `check-host.sh` | End of Phase A | The judge's **name** and **key** (the key is a secret: send it over a private channel, never in a ticket or group chat) |

Never send private keys (WireGuard `privatekey`, the judge key) back anywhere.

## Fixed values for this deployment

| Setting | Value |
| --- | --- |
| Tunnel subnet | `10.77.0.0/24` |
| Site host inside the tunnel | `10.77.0.1` (bridge and problem data) |
| This judge inside the tunnel | `10.77.0.2` (a second judge gets `10.77.0.3`, and so on: ask the site admin) |
| Bridge | `bridge.qhhoj.internal:9990` (gRPC over TLS) |
| Problem data | NFS `10.77.0.1:/srv/qhhoj/problem-data`, mounted read-only at `/srv/qhhoj/problem-data` |
| Day-one languages | `C,C11,CLANG,CPP03,CPP11,CPP14,CPP17,CPP20,CPP23,CLANGX,PY3,PYPY3,PAS,JAVA,JAVA8,KOTLIN,RUBY,D,LUA,ADA,GAS64,TEXT` |

## Phase A: prepare the machine (on your own)

### 1. Kernel and timing

The judge measures CPU time, so the machine must give stable timings.

```sh
apt update && apt full-upgrade -y
apt install -y unattended-upgrades chrony linux-tools-common "linux-tools-$(uname -r)"

# swap off, now and after reboot
swapoff -a && sed -i '/\sswap\s/s/^/#/' /etc/fstab

# SMT (hyper-threading) off, now and after reboot
echo off > /sys/devices/system/cpu/smt/control
sed -i 's/^GRUB_CMDLINE_LINUX="\(.*\)"/GRUB_CMDLINE_LINUX="\1 nosmt"/' /etc/default/grub && update-grub

cat > /etc/sysctl.d/90-qhhoj-judge.conf <<'EOF'
kernel.randomize_va_space = 2
kernel.apparmor_restrict_unprivileged_userns = 1
kernel.core_pattern = core
EOF
sysctl --system
```

If `/sys/devices/system/cpu/smt/control` says `notsupported`, the CPU (or the VM) has no SMT: skip that line.
Check cgroups v2: `test -f /sys/fs/cgroup/cgroup.controllers && echo ok`.

Pin the CPU frequency and turn transparent huge pages off at every boot:

```sh
cat > /etc/systemd/system/qhhoj-judge-tuning.service <<'EOF'
[Unit]
Description=Judge host CPU and memory tuning (THP off, performance governor, boost off)
Before=qhhoj-judge.service isolate.service
[Service]
Type=oneshot
RemainAfterExit=yes
ExecStart=/bin/sh -c 'echo never > /sys/kernel/mm/transparent_hugepage/enabled; echo never > /sys/kernel/mm/transparent_hugepage/defrag; echo 0 > /sys/kernel/mm/transparent_hugepage/khugepaged/defrag'
ExecStart=-/usr/bin/cpupower frequency-set -g performance
ExecStart=/bin/sh -c 'if [ -w /sys/devices/system/cpu/cpufreq/boost ]; then echo 0 > /sys/devices/system/cpu/cpufreq/boost; fi; if [ -w /sys/devices/system/cpu/intel_pstate/no_turbo ]; then echo 1 > /sys/devices/system/cpu/intel_pstate/no_turbo; fi'
[Install]
WantedBy=multi-user.target
EOF
systemctl daemon-reload && systemctl enable --now qhhoj-judge-tuning.service
```

(`cpupower` has no governor to set on many VMs; the leading `-` lets that line fail.) Reboot once now and
check `cat /sys/devices/system/cpu/smt/control` says `off` (or `notsupported`) and `swapon --show` prints
nothing.

### 2. isolate 2.6

```sh
apt install -y build-essential libcap-dev libseccomp-dev libsystemd-dev pkg-config git
git clone https://github.com/ioi/isolate /usr/local/src/isolate
cd /usr/local/src/isolate
git checkout --detach 19ec4bd3fac386ae4cb95a7d43d8188710154661   # tag v2.6, pinned
make isolate isolate-cg-keeper && make install
cp systemd/isolate.service systemd/isolate.slice /etc/systemd/system/
useradd --system isolate
usermod --add-subuids 200000-265535 --add-subgids 200000-265535 isolate
systemctl daemon-reload && systemctl enable --now isolate.service
```

In `/usr/local/etc/isolate` keep `box_root = /var/local/lib/isolate` (local disk, never NFS) and
`cg_root = auto:/run/isolate/cgroup`. Then:

```sh
isolate --version                       # 2.6
ls -l /usr/local/bin/isolate            # -rwsr-xr-x root root (setuid root: keep it so)
isolate-check-environment               # see below
isolate --cg -b 0 --init && isolate --cg -b 0 --cleanup
```

`isolate-check-environment` reports address-space randomisation (ASLR) as a problem: that one is expected
(we require ASLR on). Every other `FAIL`, `CAUTION` or `WARNING` must be fixed (it means step 1 is incomplete).

### 3. Compilers and runtimes

```sh
apt install -y gcc g++ clang fp-compiler python3 pypy3 default-jdk-headless ruby gdc lua5.4 gnat binutils unzip

# Kotlin 2.1.21 (pinned by checksum)
curl -fsSL -o /tmp/kotlinc.zip https://github.com/JetBrains/kotlin/releases/download/v2.1.21/kotlin-compiler-2.1.21.zip
echo "1ba08a8b45da99339a0601134cc037b54cf85e9bc0edbe76dcbd27c2d684a977  /tmp/kotlinc.zip" | sha256sum -c
rm -rf /usr/local/lib/kotlinc && unzip -q /tmp/kotlinc.zip -d /usr/local/lib    # the judge runs /usr/local/lib/kotlinc/bin/kotlinc
JAVA_HOME=/usr/lib/jvm/default-java /usr/local/lib/kotlinc/bin/kotlinc -version
```

### 4. Judge user and directories

```sh
useradd --system --no-create-home --shell /usr/sbin/nologin qhhoj-judge
install -d -m 0755 /opt/qhhoj-judge/releases /etc/qhhoj-judge /srv/qhhoj/problem-data
```

### 5. WireGuard tunnel to the site

```sh
apt install -y wireguard
umask 077
wg genkey | tee /etc/wireguard/privatekey | wg pubkey > /etc/wireguard/publickey
cat /etc/wireguard/publickey      # send THIS (public) key to the site admin, with your public IP
```

When the site admin sends back the site's public key, write `/etc/wireguard/wg0.conf` (mode 600):

```ini
[Interface]
Address = 10.77.0.2/24
PrivateKey = <contents of /etc/wireguard/privatekey>

[Peer]
PublicKey = <site public key from the site admin>
Endpoint = <SITE_PUBLIC_IP>:51820
AllowedIPs = 10.77.0.1/32
PersistentKeepalive = 25
```

```sh
systemctl enable --now wg-quick@wg0
wg show                          # "latest handshake" appears within a few seconds
ping -c 3 10.77.0.1
echo "10.77.0.1 bridge.qhhoj.internal" >> /etc/hosts
```

`PersistentKeepalive` matters: the judge firewall (step 9) accepts only traffic the judge started, so the
judge keeps the tunnel open from its side.

### 6. Problem data (read-only NFS through the tunnel)

Wait until the site admin confirms the export, then:

```sh
apt install -y nfs-common
echo "10.77.0.1:/srv/qhhoj/problem-data /srv/qhhoj/problem-data nfs4 ro,nosuid,nodev,noexec,hard,vers=4.2,actimeo=1,_netdev,x-systemd.requires=wg-quick@wg0.service 0 0" >> /etc/fstab
systemctl daemon-reload && mount -a
ls /srv/qhhoj/problem-data | head          # problem codes appear
touch /srv/qhhoj/problem-data/x 2>&1       # must fail: "Read-only file system"
```

`actimeo=1` is required: the judge waits at most 10 s for a problem's new data version to become visible.

### 7. Bridge certificate authority

Save the `bridge-ca.pem` the site admin sent:

```sh
install -m 0644 bridge-ca.pem /etc/qhhoj-judge/bridge-ca.pem
openssl s_client -connect bridge.qhhoj.internal:9990 -servername bridge.qhhoj.internal \
  -CAfile /etc/qhhoj-judge/bridge-ca.pem -verify_return_error </dev/null 2>&1 | grep -E "Verify return code"
```

It must print `Verify return code: 0 (ok)` (only once the site's bridge is running; if it is not yet, the
site admin will tell you, and you repeat this in Phase B).

### 8. Install the release

Download the release from the private judge-v2 repository (needs `gh auth login` with read access to
`qhhoj/judge-v2`; or take the files the site admin sent), verify and unpack it:

```sh
VER=<version>                                     # e.g. 2.0.1
cd /tmp && gh release download v$VER -R qhhoj/judge-v2
# built by this repository's CI from the signed tag v$VER (a recent gh: 2.103 works); refuse the release if it fails
gh attestation verify qhhoj-judge-$VER.tar.gz -R qhhoj/judge-v2 \
  --cert-identity "https://github.com/qhhoj/judge-v2/.github/workflows/ci.yml@refs/tags/v$VER"
sha256sum -c qhhoj-judge-$VER.tar.gz.sha256       # OK
tar -xzf qhhoj-judge-$VER.tar.gz -C /opt/qhhoj-judge/releases/
mv /opt/qhhoj-judge/releases/qhhoj-judge-$VER /opt/qhhoj-judge/releases/$VER
(cd /opt/qhhoj-judge/releases/$VER && sha256sum -c SHA256SUMS)     # every line OK
ln -sfn releases/$VER /opt/qhhoj-judge/current

install -m 0644 /opt/qhhoj-judge/current/packaging/config.example.toml /etc/qhhoj-judge/config.toml
cp /opt/qhhoj-judge/current/packaging/qhhoj-judge.service /etc/systemd/system/
systemctl daemon-reload                           # do NOT enable or start it yet
```

Edit `/etc/qhhoj-judge/config.toml`:

- `name = "<judge name from the site admin>"` (you can set it in Phase B)
- `bridge_url = "https://bridge.qhhoj.internal:9990"`
- leave `[tls] server_name = "bridge.qhhoj.internal"` and `ca_file` as they are
- leave `slots` and `cores` unset: the judge uses one slot per physical core, minus one core for itself

Then set up [box isolation](#box-isolation-cpuset-and-disk-quota): cpuset pinning and the box disk quota.

### 9. Firewall

```sh
apt install -y nftables
cp /opt/qhhoj-judge/current/packaging/nftables-judge.conf /etc/nftables.conf
```

Edit the `define` lines at the top of `/etc/nftables.conf`:

| define | value |
| --- | --- |
| `SITE_PUBLIC` | the site public IP (from the site admin) |
| `WG_PORT` | `51820` |
| `BRIDGE` | `10.77.0.1` |
| `BRIDGE_PORT` | `9990` |
| `ADMIN_NET` | the range you SSH from (**get this right, or you lock yourself out**) |
| `RESOLVER` | the machine's DNS server (`resolvectl status` shows it; with systemd-resolved use the upstream server) |

Keep a second SSH session open while loading it, so a mistake can be undone:

```sh
nft -c -f /etc/nftables.conf                      # syntax check, changes nothing
nft -f /etc/nftables.conf && systemctl enable nftables
nft list chain inet filter output | grep 'policy drop'
```

From the second session, open a **new** SSH connection to prove you can still get in. If not, run
`nft flush ruleset` in the old session.

### 10. Verify

```sh
DAY_ONE_LANGUAGES=$(sed -n 's/^| Day-one languages | `\(.*\)` |$/\1/p' /opt/qhhoj-judge/current/DEPLOY.md)   # the row of "Fixed values"
sudo -u qhhoj-judge /opt/qhhoj-judge/current/qhhoj-judge selftest \
  --config /etc/qhhoj-judge/config.toml --require "$DAY_ONE_LANGUAGES"       # last line ends in ok
bash /opt/qhhoj-judge/current/packaging/check-host.sh --bridge bridge.qhhoj.internal:9990
systemctl show qhhoj-judge -p NoNewPrivileges                               # NoNewPrivileges=no
```

`check-host.sh` must print `ok` on every line except two, which are expected now and become `ok` in Phase B:
`judge key is 0400 root` (no key yet), and `TLS to bridge.qhhoj.internal:9990` if the bridge is not running
yet. `selftest` judges every program of `tests/programs/manifest.toml` (shipped in the release) on this host and
prints one line per row; a `MISMATCH` row is a status, on any case, that differs here. It uses boxes 900-901 pinned
to the last core no slot owns (core 0 or 1 when slots run on 2-5) at nice 10, so it can run while the judge is live
and never shares a slot's core; it refuses to start when a box or `--core` is a slot's (`--box-base`, `--core`,
`--nice` change them). It judges with the manifest's own limits (1 s, 64 MiB, a 64 KiB output limit) and no disk
quota, because its expectations hold only under those: it does not check the config's `[limits]` or quota. If a language fails `selftest`, fix it (usually a missing package) before going on: every judge must
offer every language in the list.

`NoNewPrivileges` must be `no`: with `yes`, isolate cannot use its setuid bit and every run fails.

Send the site admin "Phase A done" with the `selftest` and `check-host.sh` output.

## Phase B: connect

The site admin creates the judge in the site's admin page and sends you its **name** and **key** (the key is
shown to them once).

```sh
install -m 0400 -o root -g root /dev/stdin /etc/qhhoj-judge/key     # paste the key, Enter, then Ctrl-D
sed -i 's/^name = .*/name = "<judge name>"/' /etc/qhhoj-judge/config.toml
bash /opt/qhhoj-judge/current/packaging/check-host.sh --bridge bridge.qhhoj.internal:9990   # all ok
systemctl enable --now qhhoj-judge
journalctl -u qhhoj-judge -n 20 --no-pager        # shows "bridge session started"
curl -s 127.0.0.1:9464/metrics | grep judge_slots_busy
```

Tell the site admin; they check that the judge shows online with its slots and submit an accepted, a
wrong-answer and a compile-error solution to see the verdicts arrive.

## Container image (optional, rootful)

Bare metal (Phase A and B above) is the primary path. For a throwaway or external judge, every push to `main` also
publishes `ghcr.io/qhhoj/judge-v2` (`:main`, `:sha-<12>`, `:vX.Y.Z` on release tags): the release binary, isolate 2.6,
the day-one language runtimes and the release files under `/opt/qhhoj-judge/current`. It does not tune the kernel:
SMT, swap, governor and THP (step 1), the firewall and the tunnel stay on the host. Run it **rootful**; rootless podman
cannot give isolate a usable `/dev`.

```sh
podman run --rm --cgroupns=private --systemd=always --cap-add=SYS_ADMIN --cap-add=NET_ADMIN \
  -v /etc/qhhoj-judge:/etc/qhhoj-judge:ro \
  -v /srv/qhhoj/problem-data:/srv/qhhoj/problem-data:ro \
  -v qhhoj-isolate:/var/local/lib/isolate \
  ghcr.io/qhhoj/judge-v2:main qhhoj-judge selftest --require "$DAY_ONE_LANGUAGES"
```

The entrypoint (`cg-setup.sh`) prepares the cgroup, then runs the command you give; with no command
the judge starts with `/etc/qhhoj-judge/config.toml` (config, `bridge-ca.pem` and the key, see steps 7 and 8; the key file is
read from the config's `key_file`, so mount it and set an absolute path). Add `--security-opt apparmor=unconfined` if the
host's AppArmor profile blocks mounts. Keep `/var/local/lib/isolate` on local disk, never NFS. A volume there passes
the start check as a filesystem of its own even when it is a directory on the host's disk: give it a size
bound or a `disk_quota` (§"Disk quota on the box root").

## Box isolation (cpuset and disk quota)

Do both on a new host in Phase A after step 8, and on a running host in an agreed deploy window. Without the
cpuset lines the judge runs with a warning. Without a disk bound it refuses to start (see below).

### CPU pinning by cpuset

The judge pins each box to its worker's core with `taskset`, which a program can undo with
`sched_setaffinity` and then slow another slot down (wrong TLEs). isolate's cgroup cpuset cannot be undone
from inside a box. isolate reads it from `box<id>.cpus` lines in `/usr/local/etc/isolate`.

```sh
cat "$(cat /run/isolate/cgroup)/cgroup.subtree_control"      # must list cpuset (isolate.service enables it)
sed -i '/^box[0-9]*\.cpus/d' /usr/local/etc/isolate           # drop old lines
/opt/qhhoj-judge/current/qhhoj-judge isolate-cpus --config /etc/qhhoj-judge/config.toml >> /usr/local/etc/isolate
systemctl restart qhhoj-judge
journalctl -u qhhoj-judge -n 50 --no-pager | grep -i pinned   # "boxes pinned to their cores by cpuset"
```

Add the lines only when `cpuset` is listed: isolate fails every run of a box with a `cpus` line when the
controller is missing. Run the two middle commands again whenever `cores`, `slots` or `box_base` change.
At start the judge checks the lines against its cores:

- no lines: warning `boxes pinned by taskset only`, the judge runs;
- a line on another core, or lines without the cpuset controller: the judge refuses to start and says why.

In the container image, mount an isolate config with the lines over `/usr/local/etc/isolate`; `cg-setup.sh`
enables cpuset when the host delegates it to the container.

### Disk quota on the box root

`--fsize` caps each file a run writes, not their total: one run can write many files and fill the disk that
the box root shares with the cache (and, on a shared host, the databases). Two layers fix it:

1. **box_root on its own filesystem** caps all boxes together and keeps `/` safe. Size it at least
   2 × slots × (headroom + 3 × the largest test file), plus 1 GiB: a checker box holds a case's input,
   output and answer at once (10 GiB for 4 slots, 256 MiB headroom and 256 MiB files).
2. **User quotas there** with `disk_quota` in the judge config cap each box.

Without a spare partition, a loop image works (it takes 10 GiB of `/`: check `df -h /` first). Box 999 below
is one the judge never owns: isolate keeps the test's 1 MB limit on that box's uid.

```sh
apt install -y quota
systemctl stop qhhoj-judge
fallocate -l 10G /var/local/isolate-box.img
mkfs.ext4 -q -O quota -E quotatype=usrquota /var/local/isolate-box.img
echo "/var/local/isolate-box.img /var/local/lib/isolate ext4 loop,nodev,nosuid,usrquota 0 2" >> /etc/fstab
systemctl daemon-reload && mount /var/local/lib/isolate
quotaon -pu /var/local/lib/isolate                         # "user quota on /var/local/lib/isolate ... is on"
isolate --cg -b 999 --init --quota=1000,100 && isolate --cg -b 999 --cleanup   # no "Cannot set disk quota"
```

The `usrquota` mount option is what enforces the limits (without it ext4 only counts usage). Then in
`/etc/qhhoj-judge/config.toml`:

```toml
[isolate]
disk_quota = { headroom_kib = 262144, headroom_inodes = 1024 }   # per box, beyond what the judge stages
```

isolate charges every file in a box to its quota, the judge's own too. So each time the judge resets a box
it sets the quota to the headroom plus what it is about to put in (program, input, answer, contestant
output) and the most the run may write (its output limit; a compile's snapshot limit). The headroom only
covers the rest: `headroom_kib` must be at least 131072 (a stderr or other file at the 64 MiB output cap,
plus 64 MiB for a compiler's temporary files), `headroom_inodes` at least 256.

`systemctl start qhhoj-judge`; the journal says `every box has a disk quota`. At start the judge refuses to run
when nothing bounds the box root:

- `disk_quota` set but isolate cannot set it (the box root has no user quotas): refused;
- no `disk_quota` and the box root on the filesystem of `/` or of `cache_dir`: refused;
- no `disk_quota` and the box root on a filesystem of its own: runs, with a warning (layer 1 only).

`[isolate] allow_unbounded_disk = true` skips the check; it is accepted only with a loopback `bridge_url`, for dev
and test stacks. isolate keeps a box uid's last limits on the filesystem: after removing `disk_quota`, recreate
the image (or clear them with `setquota`).

### Verify on the host

Run the isolate-gated tests once on the judge host after these steps, before relying on either layer. Build
them where cargo is (`cargo test -p judge-sandbox --features isolate --no-run` and the same for
`-p judge-grader` print the test binaries' paths), copy `isolate_box-<hash>` and `run_case_isolate-<hash>`
over, and with the judge stopped (`CORE` is one of the judge's worker cores, from `isolate-cpus`):

```sh
CORE=1
grep -q '^box511\.cpus' /usr/local/etc/isolate || echo "box511.cpus = $CORE" >> /usr/local/etc/isolate
JUDGE_TEST_BOX_BASE=500 ./isolate_box-<hash> --test-threads=1 cpuset quota headroom
JUDGE_TEST_BOX_BASE=500 ./run_case_isolate-<hash> --test-threads=1 headroom
sed -i '/^box511\.cpus/d' /usr/local/etc/isolate
```

Boxes 500 and up must be ones the judge does not own (`box_base` is below them). A line `skip:` means that
layer is not set up; `test result: ok` with no `skip:` lines is the pass.

## Operating the judge

- **Logs:** `journalctl -u qhhoj-judge -f`.
- **Metrics** (local only): `curl -s 127.0.0.1:9464/metrics` (`judge_assignments_total`,
  `judge_sandbox_failures_total`, `judge_reconnects_total`, `judge_slots_busy`).
- **Update to a new release:** unpack it into `/opt/qhhoj-judge/releases/<new>` as in step 8, run
  `sha256sum -c SHA256SUMS` and `selftest` (step 10) from the new directory (`--languages` and `--testlib` of the
  new release; its programs are found next to its binary), then
  `ln -sfn releases/<new> /opt/qhhoj-judge/current && systemctl restart qhhoj-judge`. A restart drains:
  it takes no new work and lets running submissions finish (up to 120 s). Agree on a time with the site
  admin; never during a contest.
- **Roll back:** point `current` at the previous release and restart.
- **Key rotation:** the site admin rotates the key (this disconnects the judge); replace
  `/etc/qhhoj-judge/key` as in Phase B and `systemctl restart qhhoj-judge`.
- **Package updates:** unattended-upgrades installs security updates. Kernel updates need a reboot: agree on
  a time, then reboot; everything comes back on its own (tunnel, mount, isolate, judge).

## Troubleshooting

| Symptom | Check |
| --- | --- |
| `wg show` has no handshake | Firewall `SITE_PUBLIC`/`WG_PORT`; the site admin added your public key and IP; `PersistentKeepalive = 25` |
| `mount` hangs | Tunnel first (`ping 10.77.0.1`); the site admin's export lists `10.77.0.2` |
| TLS check fails with "verify error" | Wrong `bridge-ca.pem`, or `/etc/hosts` lacks `bridge.qhhoj.internal` |
| Journal: `Judge key check failed` / no session | Wrong key or `name`; the key file must be exactly the key, mode 0400 root |
| Every submission is "internal error" | `NoNewPrivileges=yes`, isolate not setuid, or `isolate.service` not running |
| One language always fails | `selftest --require <LANG>` names the missing compiler |
| Judge refuses to start: `isolate config pins box …` or `… no cpuset controller` | [CPU pinning by cpuset](#cpu-pinning-by-cpuset): regenerate the `box<id>.cpus` lines, or check the controller |
| After a reboot nothing works | `systemctl status wg-quick@wg0 isolate qhhoj-judge-tuning qhhoj-judge`; `findmnt /srv/qhhoj/problem-data` |

## Appendix: the site admin's side (for reference)

Done on the site host, not by you:

1. WireGuard: `wg0` with `Address = 10.77.0.1/24`, `ListenPort = 51820`, one `[Peer]` per judge
   (`PublicKey` = the judge's, `AllowedIPs = 10.77.0.2/32`); `ufw allow from <judge public IP> to any port
   51820 proto udp`.
2. NFS: `/etc/exports` line `/srv/qhhoj/problem-data 10.77.0.2(ro,all_squash,anonuid=987,anongid=987,no_subtree_check,sec=sys)`;
   `exportfs -ra`; ufw allows 2049/tcp and 9990/tcp only on `wg0`.
3. Bridge: `JUDGE_BRIDGE_PORT=9990`, TLS certificate for `bridge.qhhoj.internal` signed by the judge CA;
   the CA's private key stays offline. Only `bridge-ca.pem` goes to judges.
4. Judge name and key: created in the admin page (Judges), sent to the deployer privately.
