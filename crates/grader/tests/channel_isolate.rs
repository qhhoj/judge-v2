#![cfg(feature = "isolate")]
//! The split channel seen from inside both boxes: which fds each box holds and what it can do with the shared
//! memory. Box offsets 60-65 (two per test). Run under `flock -o` (CONTRIBUTING.md): a channel run refuses to
//! start while the test process holds an inheritable fd.
mod common;

use common::{language_or_skip, registry, sandbox};
use judge_grader::compile::{BuildOutcome, CompileJob, build, program_for};
use judge_grader::program::Program;
use judge_grader::session::channel::RingChannel;
use judge_grader::session::files::stage_session_page;
use judge_grader::session::run::{
    ChannelPlan, MemberSpec, host_deadline, ignoring_sigpipe, spawn_session, wait_all,
};
use judge_sandbox::IsolateBox;
use judge_sandbox::spec::{Limits, RunSpec, Stdio};
use std::path::Path;

const PROBE: &str = include_str!("fixtures/channel/fd_probe.c");
const PROBE_OUT: &str = "probe.out";

/// What the probe printed in each box.
struct Probe {
    jury: String,
    contestant: String,
}

/// Compiles the probe in `sandbox` into `into`; `None` (after `skip:`) when C11 is not installed.
fn build_probe(sandbox: &mut IsolateBox, into: &Path) -> Option<Program> {
    let registry = registry();
    let language = language_or_skip(&registry, "C11")?;
    let outcome = build(sandbox, &CompileJob::for_language(&language, PROBE.as_bytes()), into).unwrap();
    assert!(matches!(outcome, BuildOutcome::Built { .. }), "{outcome:?}");
    Some(program_for(&language, into, PROBE.as_bytes()))
}

/// One process per box (`fork` must fail), stdout to a box file, the channel on fds 3-5. The file size limit is
/// above twice the shared memory, so the seals, not `--fsize` (SIGXFSZ), are what refuse its resizing.
fn probe_run(argv: Vec<String>) -> RunSpec {
    RunSpec {
        argv,
        limits: Limits::for_solution(2.0, 262_144, 1, 16 << 20),
        stdio: Stdio { stdin: None, stdout: Some(PROBE_OUT.into()), stderr: Some("/dev/null".into()) },
        env: vec![],
        dirs: vec![],
        inherit_fds: true,
    }
}

/// Runs the probe as manager (jury box `offset + 1`, with the session page) and as contestant (box `offset`) of
/// one ring session, and returns each side's stdout.
fn run_probe(offset: u32) -> Option<Probe> {
    let dirs: Vec<_> = (0..2).map(|_| tempfile::tempdir().unwrap()).collect();
    let mut contestant_box = sandbox(offset, dirs[0].path());
    let mut jury_box = sandbox(offset + 1, dirs[0].path());
    let program = build_probe(&mut contestant_box, &dirs[1].path().join("probe"))?;
    for sandbox in [&mut contestant_box, &mut jury_box] {
        sandbox.reset().unwrap();
        sandbox.put_tree(&program.dir).unwrap();
    }
    stage_session_page(&jury_box).unwrap();
    let manager_run = probe_run(ignoring_sigpipe(program.run.clone()));
    let contestant_run = probe_run(program.run.clone());
    let mut session = spawn_session(
        &MemberSpec { sandbox: &jury_box, run: &manager_run },
        &MemberSpec { sandbox: &contestant_box, run: &contestant_run },
        ChannelPlan::Ring(RingChannel::create().unwrap()),
    )
    .unwrap();
    wait_all(&mut session, host_deadline(&manager_run.limits)).unwrap();
    let report =
        |sandbox: &IsolateBox| String::from_utf8(sandbox.read_output(PROBE_OUT, 1 << 16).unwrap()).unwrap();
    Some(Probe { jury: report(&jury_box), contestant: report(&contestant_box) })
}

/// The result the probe printed for `key`.
fn value<'a>(report: &'a str, key: &str) -> &'a str {
    let found = report.lines().find_map(|line| line.split_once(' ').filter(|(name, _)| *name == key));
    found.map(|(_, result)| result.trim()).unwrap_or_else(|| panic!("{key}: {report}"))
}

#[test]
fn each_box_sees_exactly_fds_0_to_5_with_the_channel_on_3_4_5() {
    let Some(probe) = run_probe(60) else { return };
    for report in [&probe.jury, &probe.contestant] {
        assert_eq!(value(report, "fds"), "0 1 2 3 4 5", "{report}");
        assert_eq!(value(report, "fd5_target"), "memfd");
        assert_eq!(value(report, "fd5_mode"), "rdwr");
        assert_eq!(value(report, "fd5_size"), (3u64 << 20).to_string());
        assert_eq!(value(report, "fd4_nonblock"), "yes");
        assert_eq!(value(report, "fd3_nonblock"), "no");
    }
}

#[test]
fn the_shared_memory_cannot_be_reopened_resized_resealed_or_rechmodded() {
    let Some(probe) = run_probe(62) else { return };
    for report in [&probe.jury, &probe.contestant] {
        assert_eq!(value(report, "reopen"), "EACCES", "{report}");
        for check in ["grow", "shrink", "add_seals", "fallocate_grow", "fchmod"] {
            assert_eq!(value(report, check), "EPERM", "{check}: {report}");
        }
        assert_eq!(value(report, "fork"), "EAGAIN", "one process per box: {report}");
    }
}

#[test]
fn only_the_jury_box_has_the_writable_session_page() {
    let Some(probe) = run_probe(64) else { return };
    assert_eq!(value(&probe.jury, "session_page"), "rw");
    assert_eq!(value(&probe.contestant, "session_page"), "absent");
}
