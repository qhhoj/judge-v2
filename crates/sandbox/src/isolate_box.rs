//! One isolate box. Every run starts from a fresh `--init` (Review Focus 1).

use crate::error::SandboxError;
use crate::meta::{Meta, parse_meta};
use crate::spec::{RunSpec, isolate_args};
use std::fs::{File, OpenOptions};
use std::io::Read;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Stdio};

#[derive(Debug, Clone)]
pub struct SandboxConfig {
    pub isolate_bin: PathBuf,
    pub taskset_bin: PathBuf,
    pub meta_dir: PathBuf,
}

impl Default for SandboxConfig {
    fn default() -> Self {
        SandboxConfig {
            isolate_bin: "/usr/local/bin/isolate".into(),
            taskset_bin: "/usr/bin/taskset".into(),
            meta_dir: "/var/cache/qhhoj-judge/meta".into(),
        }
    }
}

pub struct IsolateBox {
    config: SandboxConfig,
    id: u32,
    core: Option<u32>,
    dir: PathBuf,
    dirty: bool,
}

fn isolate(config: &SandboxConfig, id: u32, action: &str) -> Result<String, SandboxError> {
    let out = Command::new(&config.isolate_bin).args(["--cg", &format!("-b{id}"), action]).output()?;
    if !out.status.success() {
        return Err(SandboxError::Isolate(String::from_utf8_lossy(&out.stderr).trim().to_string()));
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// Cleans a box this process may have left behind (daemon start, after a crash).
pub fn cleanup_box(config: &SandboxConfig, id: u32) -> Result<(), SandboxError> {
    isolate(config, id, "--cleanup").map(|_| ())
}

impl IsolateBox {
    pub fn open(config: &SandboxConfig, id: u32, core: Option<u32>) -> Result<Self, SandboxError> {
        std::fs::create_dir_all(&config.meta_dir)?;
        cleanup_box(config, id)?;
        let root = isolate(config, id, "--init")?;
        Ok(IsolateBox { config: config.clone(), id, core, dir: Path::new(&root).join("box"), dirty: false })
    }

    pub fn reset(&mut self) -> Result<(), SandboxError> {
        cleanup_box(&self.config, self.id)?;
        let root = isolate(&self.config, self.id, "--init")?;
        self.dir = Path::new(&root).join("box");
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

    pub fn command(&self, spec: &RunSpec) -> Result<Command, SandboxError> {
        if self.dirty {
            return Err(SandboxError::Dirty);
        }
        let args = isolate_args(self.id, spec, &self.meta_path());
        let mut command = match self.core {
            Some(core) => {
                let mut c = Command::new(&self.config.taskset_bin);
                c.args(["-c", &core.to_string()]).arg(&self.config.isolate_bin);
                c
            }
            None => Command::new(&self.config.isolate_bin),
        };
        command.args(args);
        Ok(command)
    }

    pub fn read_meta(&self) -> Result<Meta, SandboxError> {
        parse_meta(&std::fs::read_to_string(self.meta_path())?)
    }

    pub fn run(&mut self, spec: &RunSpec) -> Result<Meta, SandboxError> {
        let mut command = self.command(spec)?;
        self.dirty = true;
        let _ = std::fs::remove_file(self.meta_path());
        let status = command.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).status()?;
        // isolate exits 1 for a program that failed and 2 for its own errors; the meta file decides.
        if status.code() == Some(2) || !self.meta_path().exists() {
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
        let _ = std::fs::remove_file(self.meta_path());
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
