//! Which fds a `--inherit-fds` run would pass into a box. isolate keeps every fd the command was started with
//! that is not close-on-exec. Rust opens everything close-on-exec, so the only such fds are ones the daemon
//! inherited from its parent or a library leaked; a channel run refuses to start while any exists.

use crate::error::SandboxError;

/// Linux `O_CLOEXEC` as `/proc/*/fdinfo` prints it (octal `flags:` line).
const O_CLOEXEC: u32 = 0o2000000;

/// Fds above 2 of this process without `O_CLOEXEC`, ascending. An fd whose flags cannot be read counts as
/// inheritable, so the caller refuses rather than guesses.
pub fn inheritable_fds() -> Result<Vec<i32>, SandboxError> {
    let mut found = Vec::new();
    for entry in std::fs::read_dir("/proc/self/fdinfo")? {
        let Some(fd) = entry?.file_name().to_str().and_then(|name| name.parse::<i32>().ok()) else {
            continue;
        };
        if fd <= 2 {
            continue;
        }
        match std::fs::read_to_string(format!("/proc/self/fdinfo/{fd}")) {
            Ok(info) if close_on_exec(&info) == Some(true) => {}
            Ok(_) => found.push(fd),
            // Closed between the listing and the read (the listing's own fd, another thread's file).
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    }
    found.sort_unstable();
    Ok(found)
}

/// The close-on-exec bit of one `/proc/*/fdinfo/<fd>` text; `None` when it has no readable `flags:` line.
fn close_on_exec(fdinfo: &str) -> Option<bool> {
    let flags = fdinfo.lines().find_map(|line| line.strip_prefix("flags:"))?;
    u32::from_str_radix(flags.trim(), 8).ok().map(|bits| bits & O_CLOEXEC != 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_close_on_exec_bit_is_read_from_the_octal_flags_line() {
        assert_eq!(close_on_exec("pos:\t0\nflags:\t02100002\nmnt_id:\t15\n"), Some(true));
        assert_eq!(close_on_exec("pos:\t0\nflags:\t0100002\nmnt_id:\t15\n"), Some(false));
        assert_eq!(close_on_exec("pos:\t0\n"), None, "no flags line: unknown, the caller refuses");
        assert_eq!(close_on_exec("flags:\tzz\n"), None);
    }
}
