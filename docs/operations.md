# Operations

Setting a judge host up from nothing is [DEPLOY.md](../DEPLOY.md). This page is how the daemon behaves once running.

## Config

`/etc/qhhoj-judge/config.toml` (`daemon/src/config.rs`; unknown keys are refused). The main keys: `name`, `bridge_url`,
`tls.ca_file`, `key_file`, `slots` (default: physical cores minus one, SMT siblings excluded), `cores`,
`problem_data_root`, `cache_dir`, `cache_max_bytes`, `languages_file`, `testlib_dir`, `metrics_listen`
(default `127.0.0.1:9464`), `box_base`, `drain_timeout_s` (120), `data_wait_ms` (10000), `[limits]`
(`default_output_limit`, `default_output_prefix`, `jvm_reserve_kib`), `[reconnect]` and `[isolate]`. A non-loopback
bridge needs `https://` and `[tls]`.

Worker *i* is pinned to core *c_i* (isolate runs under `taskset -c c_i`); pinned cores and no SMT siblings keep CPU
time stable.

## Host

- Dedicated judge host, swap off (memory accounting), SMT off or siblings excluded, cgroups v2, isolate 2.x with its
  `isolate.service`, `isolate-check-environment` clean. `packaging/check-host.sh` checks all of it.
- `kernel.randomize_va_space=2` is the one accepted `isolate-check-environment` waiver. Everything else it checks (THP,
  `core_pattern`, governor, boost) is set to pass.
- The daemon runs as `qhhoj-judge` and calls the setuid isolate, so `NoNewPrivileges` is off. The unit compensates:
  `ProtectSystem=strict`, `ProtectHome`, `PrivateTmp`, `RestrictSUIDSGID`, `RestrictAddressFamilies`, `UMask=0077`.
  `packaging/check-unit.sh` (a CI step) keeps the directives that setuid isolate, its cgroups and namespaces, and the
  JVM/PyPy JITs need.
- `problem_data_root` is mounted read-only (NFS `ro,nosuid,nodev,noexec` for remote judges). The host firewall lets
  egress through only to the bridge (and NTP).
- A host whose language paths differ from the shipped `languages.toml` gets its own file from
  `qhhoj-judge selftest --host-languages`, named by `languages_file`. Language commands use paths isolate can reach
  (no `/etc/alternatives`).

## Secrets

The judge key is a file at mode 0400 passed with systemd `LoadCredential=`. It is never logged and is sent only in TLS
metadata; the bridge stores only its sha256. Sources are never logged, and compiled binaries are deleted at cleanup.

## Start

At start the daemon loads the config and logs, probes which languages this host can run (a language whose compiler is
missing is not advertised in `Hello.runtimes`), creates fresh cache directories, cleans every box it owns, then dials the
bridge. `qhhoj-judge selftest` judges every row of the conformance manifest (`tests/programs/manifest.toml`) on the host and is
the check that a host is ready (`--require` names languages that must pass).

## Drain

On SIGTERM the daemon sends `Draining` (the bridge stops assigning to it) and finishes running assignments for up to
`drain_timeout_s`, then exits; unfinished work follows the grace path in [protocol.md](protocol.md). A draining judge
still accepts an `Assign` that crossed its `Draining` message while a slot is free; it refuses only when no slot is free.

## Metrics

Prometheus text on `metrics_listen` (loopback; scrape it through the host's exporter setup or an SSH tunnel):
`judge_assignments_total{outcome}`, `judge_case_seconds`, `judge_compile_seconds`, `judge_slots_busy`,
`judge_outbox_bytes`, `judge_reconnects_total`, `judge_sandbox_failures_total{kind}`, `judge_cache_hits_total`,
`judge_data_wait_seconds`.

## Logs

JSON lines to stdout, which journald stores, with fields such as `judge`, `submission_id`, `generation`, `seq`, `box`,
`lang` and `outcome`. No sources and no keys.

## Update and rollback

1. Unpack the release tarball (or pull the image) next to the running one.
2. Run `qhhoj-judge selftest --config ... --require <the day-one languages>` and confirm every line is `ok`
   (the list is in DEPLOY.md).
3. Point the `current` symlink at the new release and `systemctl restart qhhoj-judge`; the restart drains the old process.
4. Roll hosts one at a time, waiting for each to show online.

Rollback: point the symlink back at the previous release and restart.
