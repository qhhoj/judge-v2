//! One isolate box. Every run starts from a fresh `--init`, so no run sees another's files (docs/grading.md#box-lifecycle).
//! A channel run (`command_with_channel`) is the only way fds above 2 reach a box.

use crate::drain::{Drain, Drained, Keep};
use crate::error::SandboxError;
use crate::memory_sampler::{MemorySample, MemorySampler};
use crate::meta::{Meta, parse_meta};
use crate::room::Room;
use crate::run_gate::{RunGate, RunPass};
use crate::spec::{READ_ONLY_DIR, RunSpec, isolate_args};
use std::ffi::OsString;
use std::fs::{File, OpenOptions};
use std::io::Read;
use std::os::fd::OwnedFd;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::time::Duration;

/// A disk quota on everything in a box (isolate `--quota`, in 1 KiB blocks); `--fsize` caps one file only.
/// isolate sets it on the box uid at `--init`, and at `--run` charges every file in `/box` to that uid, the
/// judge's own too. So each reset gets the headroom here plus the `Room` its caller stages and expects.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DiskQuota {
    pub headroom_kib: u32,
    pub headroom_inodes: u32,
}

/// isolate refuses numbers above a C `int`.
const ISOLATE_MAX_NUMBER: u64 = i32::MAX as u64;

/// The headroom of a contestant run's box (`IsolateBox::reset_for_contestant`), or the configured one when that
/// is smaller: the run's own files are all in its `Room`, so this only covers file system overhead.
const CONTESTANT_HEADROOM: DiskQuota = DiskQuota { headroom_kib: 4096, headroom_inodes: 64 };

impl DiskQuota {
    /// isolate's `--quota` for a box that also holds `room`.
    fn arg(&self, room: Room) -> String {
        let total = |headroom: u32, more: u64| more.saturating_add(headroom.into()).min(ISOLATE_MAX_NUMBER);
        format!("--quota={},{}", total(self.headroom_kib, room.kib), total(self.headroom_inodes, room.inodes))
    }

    fn for_contestant(&self) -> DiskQuota {
        DiskQuota {
            headroom_kib: self.headroom_kib.min(CONTESTANT_HEADROOM.headroom_kib),
            headroom_inodes: self.headroom_inodes.min(CONTESTANT_HEADROOM.headroom_inodes),
        }
    }
}

#[derive(Debug, Clone)]
pub struct SandboxConfig {
    pub isolate_bin: PathBuf,
    pub taskset_bin: PathBuf,
    /// isolate's meta files, and `dev/`: the `/dev` every box gets (`box_dev`).
    pub meta_dir: PathBuf,
    /// `None`: no quota (isolate's box root needs user quotas for one).
    pub disk_quota: Option<DiskQuota>,
    /// Held by every run of a box with this config (`run_gate`); clones share it.
    pub gate: Arc<RunGate>,
    /// isolate's own config: where the box cgroups are (`isolate_config::box_memory_stat`).
    pub isolate_config: PathBuf,
}

impl Default for SandboxConfig {
    fn default() -> Self {
        SandboxConfig {
            isolate_bin: "/usr/local/bin/isolate".into(),
            taskset_bin: "/usr/bin/taskset".into(),
            meta_dir: "/var/cache/qhhoj-judge/meta".into(),
            disk_quota: None,
            gate: Arc::default(),
            isolate_config: "/usr/local/etc/isolate".into(),
        }
    }
}

/// The three channel fds a box receives, in box fd order: 3 = what the peer writes (read end), 4 = what the
/// peer reads (write end), 5 = the shared memory of a ring channel (read-write; `/dev/null` for plain pipes).
/// In a ring channel fds 3 and 4 are doorbells; in a pipe channel they carry the frames.
pub struct ChannelFds {
    pub from_peer: OwnedFd,
    pub to_peer: OwnedFd,
    pub shared: OwnedFd,
}

/// The mover: the command starts with the channel fds as its 0/1/2, moves them to 3/4/5 by duplication (so fd 5
/// keeps the shared memory's read-write mode; reopening it by path would fail on its 0600 mode), points 0/1/2 at
/// /dev/null, then execs the isolate command line. isolate's own `--stdin/--stdout/--stderr` give the program
/// its stdio.
const CHANNEL_MOVER: &str = r#"exec 3<&0 4>&1 5<&2 </dev/null >/dev/null 2>/dev/null; exec "$@""#;

pub struct IsolateBox {
    config: SandboxConfig,
    id: u32,
    core: Option<u32>,
    dir: PathBuf,
    dirty: bool,
    /// Files were put in with `put_read_only` since the last reset.
    read_only: bool,
}

/// isolate's error when the box root cannot take a quota: "quotas have not been enabled", or `quotactl` failed.
const NO_DISK_QUOTA: &str = "Cannot set disk quota";

fn isolate(config: &SandboxConfig, id: u32, args: &[String]) -> Result<String, SandboxError> {
    let out = Command::new(&config.isolate_bin)
        .args(["--cg".to_string(), format!("-b{id}")])
        .args(args)
        .output()?;
    if !out.status.success() {
        return Err(SandboxError::Isolate(String::from_utf8_lossy(&out.stderr).trim().to_string()));
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// Cleans a box this process may have left behind (daemon start, after a crash).
pub fn cleanup_box(config: &SandboxConfig, id: u32) -> Result<(), SandboxError> {
    isolate(config, id, &["--cleanup".into()]).map(|_| ())
}

/// A fresh box, with a disk quota of `headroom` plus `room`. Returns the box's `/box` directory.
fn init_box(
    config: &SandboxConfig,
    id: u32,
    headroom: Option<DiskQuota>,
    room: Room,
) -> Result<PathBuf, SandboxError> {
    let mut args: Vec<String> = headroom.iter().map(|q| q.arg(room)).collect();
    args.push("--init".into());
    Ok(Path::new(&isolate(config, id, &args)?).join("box"))
}

/// False when isolate cannot set `config.disk_quota` under its box root (no user quotas there).
/// Opens box `id` with the quota and cleans it again, also when the quota was refused (isolate leaves the box).
pub fn disk_quota_works(config: &SandboxConfig, id: u32) -> Result<bool, SandboxError> {
    match IsolateBox::open(config, id, None) {
        Ok(_) => Ok(true),
        Err(SandboxError::Isolate(message)) if message.contains(NO_DISK_QUOTA) => {
            let _ = cleanup_box(config, id);
            Ok(false)
        }
        Err(error) => Err(error),
    }
}

impl IsolateBox {
    pub fn open(config: &SandboxConfig, id: u32, core: Option<u32>) -> Result<Self, SandboxError> {
        std::fs::create_dir_all(&config.meta_dir)?;
        crate::box_dev::prepare(&box_dev_dir(config))?;
        cleanup_box(config, id)?;
        let dir = init_box(config, id, config.disk_quota, Room::default())?;
        Ok(IsolateBox { config: config.clone(), id, core, dir, dirty: false, read_only: false })
    }

    pub fn reset(&mut self) -> Result<(), SandboxError> {
        self.reset_with_room(Room::default())
    }

    /// A fresh box whose disk quota also holds `room`: the files about to be put in and what the run may write.
    pub fn reset_with_room(&mut self, room: Room) -> Result<(), SandboxError> {
        self.reset_to(self.config.disk_quota, room)
    }

    /// A fresh box for a contestant run: its disk quota is `room` (every file the run is meant to keep on disk)
    /// and a few MiB, not the configured headroom.
    pub fn reset_for_contestant(&mut self, room: Room) -> Result<(), SandboxError> {
        self.reset_to(self.config.disk_quota.map(|q| q.for_contestant()), room)
    }

    fn reset_to(&mut self, headroom: Option<DiskQuota>, room: Room) -> Result<(), SandboxError> {
        cleanup_box(&self.config, self.id)?;
        self.remove_read_only()?;
        self.dir = init_box(&self.config, self.id, headroom, room)?;
        self.dirty = false;
        Ok(())
    }

    pub fn id(&self) -> u32 {
        self.id
    }

    /// The run gate this box's runs hold (`run_gate`).
    pub fn gate(&self) -> &Arc<RunGate> {
        &self.config.gate
    }

    /// A shared pass for one run of this box: the caller holds it until the run has ended.
    pub fn run_pass(&self) -> Result<RunPass, SandboxError> {
        self.config.gate.shared()
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    fn meta_path(&self) -> PathBuf {
        self.config.meta_dir.join(format!("{}.meta", self.id))
    }

    /// Host path of a plain file name inside `/box`: one normal segment, nothing else.
    fn box_path(&self, name: &str) -> Result<PathBuf, SandboxError> {
        let plain =
            matches!(Path::new(name).components().next(), Some(Component::Normal(part)) if part == name);
        if !plain {
            return Err(SandboxError::BadName(name.to_string()));
        }
        let root = self.dir.canonicalize()?;
        let target = root.join(name);
        if target.parent() != Some(root.as_path()) {
            return Err(SandboxError::BadName(name.to_string()));
        }
        Ok(target)
    }

    /// Copies a host file into the box. `src` is trusted (the daemon's own files); `name` is not.
    pub fn put(&self, name: &str, src: &Path, executable: bool) -> Result<(), SandboxError> {
        let target = self.box_path(name)?;
        write_new(&target, &mut File::open(src)?, if executable { 0o755 } else { 0o644 })
    }

    /// Puts a copy of a host file where the run can read it but not change it, and the box file `name` as a
    /// symlink to it; returns the copy's host path. The copy is the judge's (mode 0444) on a read-only mount
    /// (`spec::READ_ONLY_DIR`), so the box can neither write it nor take it over, whereas isolate gives every
    /// file in `/box` to the box uid. `src` is trusted (the daemon's own files); `name` is not.
    pub fn put_read_only(&mut self, name: &str, src: &Path) -> Result<PathBuf, SandboxError> {
        let target = self.box_path(name)?;
        let copy = self.make_read_only_dir()?.join(name);
        write_new(&copy, &mut File::open(src)?, 0o444)?;
        if std::fs::symlink_metadata(&target).is_ok() {
            std::fs::remove_file(&target)?;
        }
        std::os::unix::fs::symlink(Path::new("/").join(READ_ONLY_DIR).join(name), &target)?;
        self.read_only = true;
        Ok(copy)
    }

    /// The host directory of this box's read-only files: in a private (0700) directory of the judge's, and
    /// itself 0711 so the box uid can open a file in it by name.
    fn read_only_dir(&self) -> PathBuf {
        self.config.meta_dir.join("read-only").join(self.id.to_string())
    }

    fn make_read_only_dir(&self) -> Result<PathBuf, SandboxError> {
        let dir = self.read_only_dir();
        let parent = dir.parent().unwrap_or(&self.config.meta_dir);
        for (path, mode) in [(parent, 0o700), (dir.as_path(), 0o711)] {
            make_dir(path)?;
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))?;
        }
        Ok(dir)
    }

    fn remove_read_only(&mut self) -> Result<(), SandboxError> {
        self.read_only = false;
        match std::fs::remove_dir_all(self.read_only_dir()) {
            Err(error) if error.kind() != std::io::ErrorKind::NotFound => Err(error.into()),
            _ => Ok(()),
        }
    }

    pub fn put_bytes(&self, name: &str, bytes: &[u8]) -> Result<(), SandboxError> {
        write_new(&self.box_path(name)?, &mut &bytes[..], 0o644)
    }

    /// Like `put_bytes`, but mode 0666: a file the box uid writes in place (the session page).
    pub fn put_writable(&self, name: &str, bytes: &[u8]) -> Result<(), SandboxError> {
        write_new(&self.box_path(name)?, &mut &bytes[..], 0o666)
    }

    /// Opens a regular file the program left in the box for reading. A symlink is refused.
    pub fn open_output(&self, name: &str) -> Result<File, SandboxError> {
        let path = self.box_path(name)?;
        // O_NOFOLLOW refuses a symlink, O_NONBLOCK keeps a FIFO from hanging the open.
        let file = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(path)
            .map_err(|_| SandboxError::BadName(name.to_string()))?;
        if !file.metadata()?.is_file() {
            return Err(SandboxError::BadName(name.to_string()));
        }
        Ok(file)
    }

    /// Gives the box file `existing` a second name (a symlink itself, never its target). Returns false, changing
    /// nothing, when `name` is taken.
    pub fn link(&self, existing: &str, name: &str) -> Result<bool, SandboxError> {
        match std::fs::hard_link(self.box_path(existing)?, self.box_path(name)?) {
            Ok(()) => Ok(true),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => Ok(false),
            Err(e) => Err(e.into()),
        }
    }

    /// The box file named `name` ignoring ASCII case: `name` itself when it is there, else the first match
    /// in name order.
    pub fn find_ignoring_case(&self, name: &str) -> Result<Option<String>, SandboxError> {
        let mut matches = Vec::new();
        for entry in std::fs::read_dir(&self.dir)? {
            let Ok(found) = entry?.file_name().into_string() else { continue };
            if found == name {
                return Ok(Some(found));
            }
            if found.eq_ignore_ascii_case(name) {
                matches.push(found);
            }
        }
        Ok(matches.into_iter().min())
    }

    /// Reads at most `cap` bytes of a regular file the program left in the box. A symlink is refused.
    pub fn read_output(&self, name: &str, cap: u64) -> Result<Vec<u8>, SandboxError> {
        let mut bytes = Vec::new();
        self.open_output(name)?.take(cap).read_to_end(&mut bytes)?;
        Ok(bytes)
    }

    pub fn put_tree(&self, src_dir: &Path) -> Result<(), SandboxError> {
        copy_tree(src_dir, &self.dir)
    }

    /// `[taskset -c <core>] isolate <args>`: the command line both `command` and `command_with_channel` run.
    /// The previous run's meta file is removed first, so a run that writes none (isolate killed by a signal, or
    /// never started) fails `read_meta` instead of being graded from old data. The spec's runtime files are read
    /// into the page cache from here (`preload`), so the box is not charged for them.
    fn isolate_command_line(&self, spec: &RunSpec) -> Result<Vec<OsString>, SandboxError> {
        if self.dirty {
            return Err(SandboxError::Dirty);
        }
        self.remove_meta()?;
        crate::preload::preload(&spec.preload);
        let mut line: Vec<OsString> = Vec::new();
        if let Some(core) = self.core {
            line.extend([self.config.taskset_bin.clone().into(), "-c".into(), core.to_string().into()]);
        }
        line.push(self.config.isolate_bin.clone().into());
        let read_only = self.read_only.then(|| self.read_only_dir());
        let args = isolate_args(
            self.id,
            spec,
            &self.meta_path(),
            (&box_dev_dir(&self.config), read_only.as_deref()),
        );
        line.extend(args.into_iter().map(OsString::from));
        Ok(line)
    }

    /// The isolate command for `spec`. A spec that inherits fds is refused: only `command_with_channel`, which
    /// checks the daemon's fds first, may start such a run.
    pub fn command(&self, spec: &RunSpec) -> Result<Command, SandboxError> {
        if spec.inherit_fds {
            return Err(SandboxError::Isolate("a run that inherits fds needs command_with_channel".into()));
        }
        let line = self.isolate_command_line(spec)?;
        let mut command = Command::new(&line[0]);
        command.args(&line[1..]);
        own_session_keyring(&mut command);
        Ok(command)
    }

    /// A command for `spec` (which must set `inherit_fds`) that gives the box `fds` as 3/4/5 and /dev/null as
    /// the command's own 0/1/2. Refuses while the daemon holds inheritable fds (`fds::inheritable_fds`): the box
    /// would receive them too. `fds` are owned by the returned command and close when it drops.
    pub fn command_with_channel(&self, spec: &RunSpec, fds: ChannelFds) -> Result<Command, SandboxError> {
        if !spec.inherit_fds {
            return Err(SandboxError::Isolate("a channel run needs RunSpec.inherit_fds".into()));
        }
        let leaked = crate::fds::inheritable_fds()?;
        if !leaked.is_empty() {
            return Err(SandboxError::InheritableFds(leaked));
        }
        let mut command = Command::new("/bin/sh");
        command.args(["-c", CHANNEL_MOVER, "sh"]).args(self.isolate_command_line(spec)?);
        command
            .stdin(Stdio::from(fds.from_peer))
            .stdout(Stdio::from(fds.to_peer))
            .stderr(Stdio::from(fds.shared));
        own_session_keyring(&mut command);
        Ok(command)
    }

    fn remove_meta(&self) -> Result<(), SandboxError> {
        match std::fs::remove_file(self.meta_path()) {
            Err(error) if error.kind() != std::io::ErrorKind::NotFound => Err(error.into()),
            _ => Ok(()),
        }
    }

    /// The meta file of the last run. isolate opens it while reading its options and writes it as it exits, so
    /// one killed by a signal leaves it missing or empty. A meta file without `time` or `status` (isolate's
    /// own errors write only `status:XX`) is refused as a sandbox failure: it would parse as a clean exit.
    pub fn read_meta(&self) -> Result<Meta, SandboxError> {
        let text = match std::fs::read_to_string(self.meta_path()) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => String::new(),
            text => text?,
        };
        let complete = text.lines().any(|line| line.starts_with("time:") || line.starts_with("status:"));
        if !complete {
            return Err(SandboxError::Meta(format!("isolate ended without a meta file: {text:?}")));
        }
        parse_meta(&text)
    }

    /// One run, holding a shared pass of the run gate from before the preload until isolate has exited.
    pub fn run(&mut self, spec: &RunSpec) -> Result<Meta, SandboxError> {
        self.run_with(spec, [Stdio::null(), Stdio::null(), Stdio::null()])
    }

    /// One run whose stdin, when given, is `stdin` (a `sealed_input` copy) and whose stdout and stderr, as
    /// `drains` asks, are pipes the judge reads (`drain`), each held to the spec's file size limit. A stream
    /// given here must not also be redirected by the spec's stdio. With `sample_memory`, the box cgroup's
    /// `memory.stat` is read while the run is live and once after it (`memory_sampler`).
    pub fn run_drained(
        &mut self,
        spec: &RunSpec,
        stdin: Option<File>,
        (drains, sample_memory): (Drains, bool),
    ) -> Result<DrainedRun, SandboxError> {
        let stdio = &spec.stdio;
        let given = [stdin.is_some(), drains.stdout.is_some(), drains.stderr.is_some()];
        let named = [&stdio.stdin, &stdio.stdout, &stdio.stderr].map(Option::is_some);
        if given.iter().zip(named).any(|(given, named)| *given && named) {
            return Err(SandboxError::Isolate(
                "a stream the judge gives is also redirected to a file".into(),
            ));
        }
        let limit = spec.limits.fsize_kib.saturating_mul(1024);
        let (stdout, stdout_pipe) = start_drain(drains.stdout, limit)?;
        let (stderr, stderr_pipe) = start_drain(drains.stderr, limit)?;
        let stdin = stdin.map_or_else(Stdio::null, Stdio::from);
        let (meta, memory) = self.run_sampled(spec, [stdin, stdout_pipe, stderr_pipe], sample_memory)?;
        Ok(DrainedRun { meta, stdout: finish_drain(stdout)?, stderr: finish_drain(stderr)?, memory })
    }

    /// The command (and with it the daemon's copies of the fds in `stdio`) is dropped on return.
    fn run_with(&mut self, spec: &RunSpec, stdio: [Stdio; 3]) -> Result<Meta, SandboxError> {
        self.run_sampled(spec, stdio, false).map(|(meta, _)| meta)
    }

    /// `run_with`, and with `sample` the box cgroup's memory samples from the start of the run to its end.
    fn run_sampled(
        &mut self,
        spec: &RunSpec,
        [stdin, stdout, stderr]: [Stdio; 3],
        sample: bool,
    ) -> Result<(Meta, Option<MemorySample>), SandboxError> {
        let _pass = self.run_pass()?;
        let mut command = self.command(spec)?;
        self.dirty = true;
        let sampler = if sample { Some(MemorySampler::start(&self.memory_stat()?)?) } else { None };
        let status = command.stdin(stdin).stdout(stdout).stderr(stderr).status();
        let memory = sampler.map(MemorySampler::finish).transpose();
        let status = status?;
        // isolate exits 1 for a program that failed and 2 for its own errors; the meta file decides.
        if status.code() == Some(2) {
            return Err(SandboxError::Isolate(format!("isolate exited with {status}")));
        }
        Ok((self.read_meta()?, memory?))
    }

    /// This box's cgroup `memory.stat`, which isolate makes at `--init` and keeps until the next one.
    fn memory_stat(&self) -> Result<PathBuf, SandboxError> {
        crate::isolate_config::box_memory_stat(&self.config.isolate_config, self.id).ok_or_else(|| {
            SandboxError::Isolate(format!("no cg_root in {}", self.config.isolate_config.display()))
        })
    }
}

/// Which of a run's stdout and stderr the judge drains, and what it keeps of each (`IsolateBox::run_drained`).
#[derive(Debug, Clone, Copy, Default)]
pub struct Drains {
    pub stdout: Option<Keep>,
    pub stderr: Option<Keep>,
}

/// A drained run: its meta file and what was kept of each stream (empty for one not drained).
#[derive(Debug)]
pub struct DrainedRun {
    pub meta: Meta,
    pub stdout: Drained,
    pub stderr: Drained,
    /// The box cgroup's memory samples, when the run asked for them.
    pub memory: Option<MemorySample>,
}

/// How long a drained stream may stay open after isolate exited (isolate kills the box's processes first).
const DRAIN_GRACE: Duration = Duration::from_secs(5);

fn start_drain(keep: Option<Keep>, limit: u64) -> Result<(Option<Drain>, Stdio), SandboxError> {
    match keep {
        Some(keep) => Drain::start(keep, limit).map(|(drain, pipe)| (Some(drain), Stdio::from(pipe))),
        None => Ok((None, Stdio::null())),
    }
}

fn finish_drain(drain: Option<Drain>) -> Result<Drained, SandboxError> {
    drain.map_or(Ok(Drained::default()), |drain| drain.finish(DRAIN_GRACE))
}

impl Drop for IsolateBox {
    fn drop(&mut self) {
        if let Err(error) = cleanup_box(&self.config, self.id) {
            eprintln!("isolate cleanup of box {} failed: {error}", self.id);
        }
        let _ = self.remove_meta();
        let _ = self.remove_read_only();
    }
}

/// Creates `target` fresh: an existing file or symlink is unlinked, never followed.
fn write_new(target: &Path, from: &mut impl Read, mode: u32) -> Result<(), SandboxError> {
    if std::fs::symlink_metadata(target).is_ok() {
        std::fs::remove_file(target)?;
    }
    let mut file = OpenOptions::new().write(true).create_new(true).mode(mode).open(target)?;
    std::io::copy(from, &mut file)?;
    file.set_permissions(std::fs::Permissions::from_mode(mode))?;
    Ok(())
}

/// Copies a program snapshot, keeping permission bits. Symlinks are refused.
fn copy_tree(src: &Path, dst: &Path) -> Result<(), SandboxError> {
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let target = dst.join(entry.file_name());
        let kind = entry.file_type()?;
        if kind.is_dir() {
            make_dir(&target)?;
            copy_tree(&entry.path(), &target)?;
        } else if kind.is_file() {
            let mode = entry.metadata()?.permissions().mode() & 0o777;
            write_new(&target, &mut File::open(entry.path())?, mode)?;
        } else {
            return Err(SandboxError::BadName(entry.file_name().to_string_lossy().into_owned()));
        }
    }
    Ok(())
}

/// The `/dev` directory of the boxes of `config` (`box_dev`).
fn box_dev_dir(config: &SandboxConfig) -> PathBuf {
    config.meta_dir.join("dev")
}

/// Starts `command` in a new session keyring of its own (`keyctl(KEYCTL_JOIN_SESSION_KEYRING, NULL)`), so a run
/// shares no keyring with the daemon or another run. A kernel without keys (ENOSYS), or a container whose
/// syscall filter refuses `keyctl` (EPERM: container runtimes refuse it with `add_key` and `request_key`, and
/// the filter holds for the box too), has no keyring to share.
fn own_session_keyring(command: &mut Command) {
    use std::os::unix::process::CommandExt;
    // SAFETY: the closure runs between fork and exec; it makes one raw syscall and reads errno, both
    // async-signal-safe, and allocates nothing.
    unsafe {
        command.pre_exec(|| {
            let join = libc::KEYCTL_JOIN_SESSION_KEYRING as libc::c_long;
            if libc::syscall(libc::SYS_keyctl, join, std::ptr::null::<libc::c_char>()) < 0 {
                let error = std::io::Error::last_os_error();
                if !matches!(error.raw_os_error(), Some(libc::ENOSYS | libc::EPERM)) {
                    return Err(error);
                }
            }
            Ok(())
        });
    }
}

/// Creates a directory, or accepts an existing real one; a symlink there is refused, never followed.
fn make_dir(target: &Path) -> Result<(), SandboxError> {
    match std::fs::symlink_metadata(target) {
        Ok(meta) if meta.is_dir() => Ok(()),
        Ok(_) => Err(SandboxError::BadName(target.display().to_string())),
        Err(_) => Ok(std::fs::create_dir(target)?),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::fd::OwnedFd;

    /// A stand-in for isolate that logs each call to `calls` and answers `--init` with a box root. A run writes
    /// no meta file, as an isolate killed before its exit does not.
    /// With a `no-quota` file beside it, an `--init` that sets a quota fails as on a box root without quotas.
    /// With an `empty-meta` file beside it, a run leaves its meta file empty, as an isolate killed after opening it.
    const FAKE_ISOLATE: &str = r#"#!/bin/sh
d=$(dirname "$0")
echo "$*" >> "$d/calls"
case "$*" in *--quota=*) if [ -e "$d/no-quota" ]; then
  echo "Cannot set disk quota: quotas have not been enabled for this filesystem" >&2; exit 2; fi ;; esac
case "$*" in *--init*) mkdir -p "$d/root/box"; echo "$d/root" ;; esac
if [ -e "$d/empty-meta" ]; then for a in "$@"; do case "$a" in --meta=*) : > "${a#--meta=}" ;; esac; done; fi
"#;

    /// Held by each test that writes a fake isolate and runs it: a child another test thread forks while
    /// the script is still open for writing keeps that fd until it execs, and running the script then fails
    /// with ETXTBSY.
    static SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn serial() -> std::sync::MutexGuard<'static, ()> {
        SERIAL.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn fake_isolate(dir: &Path, quota: Option<DiskQuota>) -> SandboxConfig {
        SandboxConfig { disk_quota: quota, ..fake_isolate_with(dir, FAKE_ISOLATE) }
    }

    fn fake_isolate_with(dir: &Path, script: &str) -> SandboxConfig {
        let bin = dir.join("isolate");
        std::fs::write(&bin, script).unwrap();
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
        SandboxConfig { isolate_bin: bin, meta_dir: dir.join("meta"), ..Default::default() }
    }

    const HEADROOM: DiskQuota = DiskQuota { headroom_kib: 196_608, headroom_inodes: 1024 };

    fn calls(dir: &Path) -> Vec<String> {
        std::fs::read_to_string(dir.join("calls")).unwrap().lines().map(String::from).collect()
    }

    #[test]
    fn every_init_sets_the_disk_quota_and_no_other_call_does() {
        let _serial = serial();
        let dir = tempfile::tempdir().unwrap();
        let config = fake_isolate(dir.path(), Some(HEADROOM));
        let mut b = IsolateBox::open(&config, 7, None).unwrap();
        b.reset().unwrap();
        drop(b);
        let calls = calls(dir.path());
        let inits: Vec<_> = calls.iter().filter(|c| c.ends_with("--init")).collect();
        assert_eq!(inits, ["--cg -b7 --quota=196608,1024 --init"; 2], "{calls:?}");
        assert_eq!(calls.iter().filter(|c| c.contains("--quota")).count(), 2, "{calls:?}");
    }

    #[test]
    fn a_reset_with_room_gets_the_headroom_plus_the_room() {
        let _serial = serial();
        let dir = tempfile::tempdir().unwrap();
        let mut b = IsolateBox::open(&fake_isolate(dir.path(), Some(HEADROOM)), 7, None).unwrap();
        b.reset_with_room(Room { kib: 153_600, inodes: 3 }).unwrap();
        b.reset().unwrap();
        let inits: Vec<_> = calls(dir.path()).into_iter().filter(|c| c.ends_with("--init")).collect();
        assert_eq!(inits[1], "--cg -b7 --quota=350208,1027 --init");
        assert_eq!(inits[2], "--cg -b7 --quota=196608,1024 --init", "the room is for one reset only");
    }

    #[test]
    fn a_contestant_reset_gets_its_room_and_four_mib_not_the_headroom() {
        let _serial = serial();
        let dir = tempfile::tempdir().unwrap();
        let mut b = IsolateBox::open(&fake_isolate(dir.path(), Some(HEADROOM)), 7, None).unwrap();
        b.reset_for_contestant(Room { kib: 153_600, inodes: 3 }).unwrap();
        let last_init = calls(dir.path()).into_iter().rfind(|c| c.ends_with("--init")).unwrap();
        assert_eq!(last_init, "--cg -b7 --quota=157696,67 --init");
    }

    #[test]
    fn a_quota_past_what_isolate_takes_is_clamped() {
        let _serial = serial();
        let dir = tempfile::tempdir().unwrap();
        let mut b = IsolateBox::open(&fake_isolate(dir.path(), Some(HEADROOM)), 7, None).unwrap();
        b.reset_with_room(Room { kib: u64::MAX, inodes: 1 << 40 }).unwrap();
        let last_init = calls(dir.path()).into_iter().rfind(|c| c.ends_with("--init")).unwrap();
        assert_eq!(last_init, "--cg -b7 --quota=2147483647,2147483647 --init");
    }

    #[test]
    fn without_a_quota_no_call_sets_one() {
        let _serial = serial();
        let dir = tempfile::tempdir().unwrap();
        let mut b = IsolateBox::open(&fake_isolate(dir.path(), None), 7, None).unwrap();
        b.reset().unwrap();
        drop(b);
        assert!(calls(dir.path()).iter().all(|c| !c.contains("--quota")), "{:?}", calls(dir.path()));
    }

    #[test]
    fn the_quota_probe_tells_a_box_root_without_quotas_from_other_failures() {
        let _serial = serial();
        let dir = tempfile::tempdir().unwrap();
        let config = fake_isolate(dir.path(), Some(HEADROOM));
        assert!(disk_quota_works(&config, 3).unwrap());
        std::fs::write(dir.path().join("no-quota"), "").unwrap();
        assert!(!disk_quota_works(&config, 3).unwrap());
        let last = calls(dir.path()).pop().unwrap();
        assert_eq!(last, "--cg -b3 --cleanup", "a refused probe leaves its box behind");
        let broken = SandboxConfig { isolate_bin: "/nonexistent/isolate".into(), ..config };
        assert!(disk_quota_works(&broken, 3).is_err(), "isolate missing is an error, not \"no quotas\"");
    }

    fn run_spec() -> RunSpec {
        RunSpec {
            argv: vec!["/bin/true".into()],
            limits: crate::spec::Limits::for_solution(1.0, 65_536, 1, 4096),
            stdio: crate::spec::Stdio::default(),
            env: vec![],
            dirs: vec![],
            preload: vec![],
            inherit_fds: false,
        }
    }

    /// A box whose previous run left a meta file that a clean verdict would come from.
    fn box_with_stale_meta(dir: &Path) -> IsolateBox {
        let b = IsolateBox::open(&fake_isolate(dir, None), 7, None).unwrap();
        std::fs::write(b.meta_path(), "status:TO\ntime:1.100\ntime-wall:1.200\n").unwrap();
        b
    }

    #[test]
    fn a_spawned_run_that_writes_no_meta_is_not_read_from_the_previous_runs_meta() {
        let _serial = serial();
        let dir = tempfile::tempdir().unwrap();
        let b = box_with_stale_meta(dir.path());
        assert!(b.command(&run_spec()).unwrap().status().unwrap().success());
        assert!(matches!(b.read_meta(), Err(SandboxError::Meta(_))), "{:?}", b.read_meta());
    }

    #[test]
    fn a_run_that_writes_no_meta_is_a_sandbox_failure() {
        let _serial = serial();
        let dir = tempfile::tempdir().unwrap();
        let result = box_with_stale_meta(dir.path()).run(&run_spec());
        assert!(matches!(result, Err(SandboxError::Meta(_))), "{result:?}");
    }

    #[test]
    fn an_empty_meta_is_a_sandbox_failure_not_a_clean_run() {
        let _serial = serial();
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("empty-meta"), "").unwrap();
        let mut b = box_with_stale_meta(dir.path());
        let result = b.run(&run_spec());
        assert!(matches!(result, Err(SandboxError::Meta(_))), "{result:?}");
        b.reset().unwrap();
        assert!(b.command(&run_spec()).unwrap().status().unwrap().success());
        assert!(matches!(b.read_meta(), Err(SandboxError::Meta(_))), "{:?}", b.read_meta());
    }

    #[test]
    fn a_truncated_meta_is_refused_and_a_complete_one_is_read() {
        let _serial = serial();
        let dir = tempfile::tempdir().unwrap();
        let b = IsolateBox::open(&fake_isolate(dir.path(), None), 7, None).unwrap();
        std::fs::write(b.meta_path(), "max-rss:1234\ncsw-voluntary:4\n").unwrap();
        assert!(matches!(b.read_meta(), Err(SandboxError::Meta(_))), "{:?}", b.read_meta());
        std::fs::write(b.meta_path(), "time:0.010\ntime-wall:0.020\nexitcode:0\n").unwrap();
        assert_eq!(b.read_meta().unwrap().exit_code, Some(0), "a clean exit has no status line");
        std::fs::write(b.meta_path(), "status:XX\nmessage:box busy\n").unwrap();
        assert_eq!(b.read_meta().unwrap().status.as_deref(), Some("XX"), "isolate's own error has no time");
    }

    /// A fake isolate that, on `--run`, writes one line per fd 0..5: `<fd> <readlink target> <flags octal>`.
    /// The shell execs nothing, so `$$` is the fake itself.
    const FAKE_ISOLATE_FDS: &str = r#"#!/bin/sh
d=$(dirname "$0")
case "$*" in *--init*) mkdir -p "$d/root/box"; echo "$d/root"; exit 0 ;; esac
case "$*" in *--run*)
  for fd in 0 1 2 3 4 5; do
    t=$(readlink /proc/$$/fd/$fd 2>/dev/null || echo closed)
    f=$(sed -n 's/^flags:[[:space:]]*//p' /proc/$$/fdinfo/$fd 2>/dev/null)
    echo "$fd $t $f" >> "$d/fds"
  done ;;
esac
"#;

    fn channel_files(dir: &Path) -> ChannelFds {
        let open = |name: &str| -> OwnedFd {
            let options = OpenOptions::new().read(true).write(true).create(true).truncate(true).clone();
            options.open(dir.join(name)).unwrap().into()
        };
        ChannelFds { from_peer: open("wait"), to_peer: open("ring"), shared: open("shared") }
    }

    #[test]
    fn a_channel_command_gives_the_box_its_fds_as_3_4_5_and_null_stdio() {
        let _serial = serial();
        let dir = tempfile::tempdir().unwrap();
        let b = IsolateBox::open(&fake_isolate_with(dir.path(), FAKE_ISOLATE_FDS), 7, None).unwrap();
        let spec = RunSpec { inherit_fds: true, ..run_spec() };
        let status = b.command_with_channel(&spec, channel_files(dir.path())).unwrap().status().unwrap();
        assert!(status.success());
        let fds = std::fs::read_to_string(dir.path().join("fds")).unwrap();
        let field = |fd: usize, column: usize| {
            fds.lines().nth(fd).unwrap().split(' ').nth(column).unwrap().to_string()
        };
        assert_eq!((0..3).map(|fd| field(fd, 1)).collect::<Vec<_>>(), ["/dev/null"; 3]);
        let real = dir.path().canonicalize().unwrap();
        let expected = ["wait", "ring", "shared"].map(|name| real.join(name).display().to_string());
        assert_eq!((3..6).map(|fd| field(fd, 1)).collect::<Vec<_>>(), expected);
        let flags5 = u32::from_str_radix(&field(5, 2), 8).unwrap();
        assert_eq!(flags5 & 0o3, 0o2, "fd 5 keeps O_RDWR: duplicated, never reopened");
    }

    #[test]
    fn a_channel_command_without_inherit_fds_is_refused() {
        let _serial = serial();
        let dir = tempfile::tempdir().unwrap();
        let b = IsolateBox::open(&fake_isolate_with(dir.path(), FAKE_ISOLATE_FDS), 7, None).unwrap();
        let refused = b.command_with_channel(&run_spec(), channel_files(dir.path()));
        assert!(matches!(refused, Err(SandboxError::Isolate(_))));
    }

    #[test]
    fn a_channel_command_on_a_core_runs_isolate_under_taskset() {
        let _serial = serial();
        let dir = tempfile::tempdir().unwrap();
        let b = IsolateBox::open(&fake_isolate_with(dir.path(), FAKE_ISOLATE_FDS), 7, Some(3)).unwrap();
        let spec = RunSpec { inherit_fds: true, ..run_spec() };
        let command = b.command_with_channel(&spec, channel_files(dir.path())).unwrap();
        let argv: Vec<String> = command.get_args().map(|a| a.to_string_lossy().into_owned()).collect();
        let tail = &argv[3..];
        assert_eq!(command.get_program(), "/bin/sh");
        assert_eq!(&tail[..4], ["/usr/bin/taskset", "-c", "3", b.config.isolate_bin.to_str().unwrap()]);
        assert!(tail.contains(&"--inherit-fds".to_string()));
    }

    #[test]
    fn a_plain_command_refuses_inherited_fds() {
        let _serial = serial();
        let dir = tempfile::tempdir().unwrap();
        let mut b = IsolateBox::open(&fake_isolate_with(dir.path(), FAKE_ISOLATE_FDS), 7, None).unwrap();
        let spec = RunSpec { inherit_fds: true, ..run_spec() };
        assert!(matches!(b.command(&spec), Err(SandboxError::Isolate(_))));
        assert!(matches!(b.run(&spec), Err(SandboxError::Isolate(_))));
        assert!(!dir.path().join("fds").exists(), "isolate never ran");
    }

    #[test]
    fn a_dirty_box_refuses_a_channel_command() {
        let _serial = serial();
        let dir = tempfile::tempdir().unwrap();
        let mut b = IsolateBox::open(&fake_isolate_with(dir.path(), FAKE_ISOLATE_FDS), 7, None).unwrap();
        b.dirty = true;
        let spec = RunSpec { inherit_fds: true, ..run_spec() };
        let refused = b.command_with_channel(&spec, channel_files(dir.path()));
        assert!(matches!(refused, Err(SandboxError::Dirty)));
    }

    #[test]
    fn a_writable_file_is_created_fresh_with_mode_0666() {
        let _serial = serial();
        let dir = tempfile::tempdir().unwrap();
        let b = IsolateBox::open(&fake_isolate(dir.path(), None), 7, None).unwrap();
        let elsewhere = dir.path().join("elsewhere");
        std::fs::write(&elsewhere, "keep").unwrap();
        std::os::unix::fs::symlink(&elsewhere, b.dir().join(".page")).unwrap();
        b.put_writable(".page", &[0u8; 4096]).unwrap();
        let meta = std::fs::symlink_metadata(b.dir().join(".page")).unwrap();
        assert!(meta.is_file());
        assert_eq!(meta.permissions().mode() & 0o777, 0o666);
        assert_eq!(std::fs::read(b.dir().join(".page")).unwrap(), vec![0u8; 4096]);
        assert_eq!(
            std::fs::read_to_string(elsewhere).unwrap(),
            "keep",
            "the link was replaced, not followed"
        );
    }
}
