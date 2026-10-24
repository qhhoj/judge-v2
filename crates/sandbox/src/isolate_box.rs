//! One isolate box. Every run starts from a fresh `--init`, so no run sees another's files (docs/grading.md#box-lifecycle).
//! A channel run (`command_with_channel`) is the only way fds above 2 reach a box.

use crate::error::SandboxError;
use crate::meta::{Meta, parse_meta};
use crate::room::Room;
use crate::spec::{RunSpec, isolate_args};
use std::ffi::OsString;
use std::fs::{File, OpenOptions};
use std::io::Read;
use std::os::fd::OwnedFd;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Stdio};

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

impl DiskQuota {
    /// isolate's `--quota` for a box that also holds `room`.
    fn arg(&self, room: Room) -> String {
        let total = |headroom: u32, more: u64| more.saturating_add(headroom.into()).min(ISOLATE_MAX_NUMBER);
        format!("--quota={},{}", total(self.headroom_kib, room.kib), total(self.headroom_inodes, room.inodes))
    }
}

#[derive(Debug, Clone)]
pub struct SandboxConfig {
    pub isolate_bin: PathBuf,
    pub taskset_bin: PathBuf,
    pub meta_dir: PathBuf,
    /// `None`: no quota (isolate's box root needs user quotas for one).
    pub disk_quota: Option<DiskQuota>,
}

impl Default for SandboxConfig {
    fn default() -> Self {
        SandboxConfig {
            isolate_bin: "/usr/local/bin/isolate".into(),
            taskset_bin: "/usr/bin/taskset".into(),
            meta_dir: "/var/cache/qhhoj-judge/meta".into(),
            disk_quota: None,
        }
    }
}

/// The three channel fds a box receives, in box fd order: 3 = the doorbell this side waits on (read end),
/// 4 = the doorbell this side rings (write end, non-blocking), 5 = the shared memory (read-write).
pub struct ChannelFds {
    pub doorbell_wait: OwnedFd,
    pub doorbell_ring: OwnedFd,
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

/// A fresh box, with a disk quota of the config's headroom plus `room`. Returns the box's `/box` directory.
fn init_box(config: &SandboxConfig, id: u32, room: Room) -> Result<PathBuf, SandboxError> {
    let mut args: Vec<String> = config.disk_quota.iter().map(|q| q.arg(room)).collect();
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
        cleanup_box(config, id)?;
        let dir = init_box(config, id, Room::default())?;
        Ok(IsolateBox { config: config.clone(), id, core, dir, dirty: false })
    }

    pub fn reset(&mut self) -> Result<(), SandboxError> {
        self.reset_with_room(Room::default())
    }

    /// A fresh box whose disk quota also holds `room`: the files about to be put in and what the run may write.
    pub fn reset_with_room(&mut self, room: Room) -> Result<(), SandboxError> {
        cleanup_box(&self.config, self.id)?;
        self.dir = init_box(&self.config, self.id, room)?;
        self.dirty = false;
        Ok(())
    }

    pub fn id(&self) -> u32 {
        self.id
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

    /// Bytes of disk the regular box file `name` holds: its allocated blocks, not its length, so the holes
    /// of a sparse file count nothing. 0 for a missing file or a symlink.
    pub fn allocated_bytes(&self, name: &str) -> u64 {
        let blocks = self.open_output(name).and_then(|file| Ok(file.metadata()?.blocks()));
        blocks.map_or(0, |blocks| blocks * 512)
    }

    /// Gives the box file `existing` a second name. Returns false, changing nothing, when `name` is taken.
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
    /// never started) fails `read_meta` instead of being graded from old data.
    fn isolate_command_line(&self, spec: &RunSpec) -> Result<Vec<OsString>, SandboxError> {
        if self.dirty {
            return Err(SandboxError::Dirty);
        }
        self.remove_meta()?;
        let mut line: Vec<OsString> = Vec::new();
        if let Some(core) = self.core {
            line.extend([self.config.taskset_bin.clone().into(), "-c".into(), core.to_string().into()]);
        }
        line.push(self.config.isolate_bin.clone().into());
        line.extend(isolate_args(self.id, spec, &self.meta_path()).into_iter().map(OsString::from));
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
            .stdin(Stdio::from(fds.doorbell_wait))
            .stdout(Stdio::from(fds.doorbell_ring))
            .stderr(Stdio::from(fds.shared));
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

    pub fn run(&mut self, spec: &RunSpec) -> Result<Meta, SandboxError> {
        let mut command = self.command(spec)?;
        self.dirty = true;
        let status = command.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).status()?;
        // isolate exits 1 for a program that failed and 2 for its own errors; the meta file decides.
        if status.code() == Some(2) {
            return Err(SandboxError::Isolate(format!("isolate exited with {status}")));
        }
        self.read_meta()
    }
}

impl Drop for IsolateBox {
    fn drop(&mut self) {
        if let Err(error) = cleanup_box(&self.config, self.id) {
            eprintln!("isolate cleanup of box {} failed: {error}", self.id);
        }
        let _ = self.remove_meta();
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
        ChannelFds { doorbell_wait: open("wait"), doorbell_ring: open("ring"), shared: open("shared") }
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
