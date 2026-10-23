//! Its own test binary, so the deliberately leaked fd lives in a process no other test shares.
use judge_sandbox::fds::inheritable_fds;
use std::os::fd::AsRawFd;

#[test]
fn a_leaked_fd_is_reported_and_a_close_on_exec_one_is_not() {
    let inherited = inheritable_fds().unwrap();
    assert!(
        inherited.is_empty(),
        "the test process itself inherited {inherited:?}: run it as CONTRIBUTING says (flock -o)"
    );
    let leaked = tempfile::tempfile().unwrap();
    let (held_read, _held_write) = std::io::pipe().unwrap(); // close-on-exec, like another slot's channel
    assert!(!inheritable_fds().unwrap().contains(&leaked.as_raw_fd()));
    rustix::io::fcntl_setfd(&leaked, rustix::io::FdFlags::empty()).unwrap();
    let found = inheritable_fds().unwrap();
    assert!(found.contains(&leaked.as_raw_fd()), "{found:?}");
    assert!(!found.contains(&held_read.as_raw_fd()));
    assert!(found.iter().all(|fd| *fd > 2), "stdio is replaced by the mover, never reported");
}
