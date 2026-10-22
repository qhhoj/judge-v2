//! Disk room a box needs for one reset's runs, on top of the configured headroom. isolate chowns `/box` to
//! the box uid at `--run`, so every file the judge puts in the box counts against the box's disk quota,
//! next to what the run itself writes.

use std::path::Path;

/// One filesystem block: every file and directory takes at least one.
const BLOCK: u64 = 4096;

/// Disk room in isolate's quota units: 1 KiB blocks and inodes.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Room {
    pub kib: u64,
    pub inodes: u64,
}

impl Room {
    /// Room for host files and directory trees about to be put in the box. A symlink given here is followed
    /// (`/proc/self/fd/N`); one inside a tree is counted as a file, never followed.
    pub fn for_files(paths: &[&Path]) -> std::io::Result<Room> {
        paths.iter().try_fold(Room::default(), |room, path| Ok(room.plus(tree_room(path)?)))
    }

    /// This room and one more file of `len` bytes: a file put in from memory, or an output the run may write.
    pub fn plus_file(self, len: u64) -> Room {
        // One block more than the data, for the file's block map.
        self.plus(Room { kib: (len.div_ceil(BLOCK) + 1) * BLOCK / 1024, inodes: 1 })
    }

    pub fn plus(self, other: Room) -> Room {
        Room { kib: self.kib.saturating_add(other.kib), inodes: self.inodes.saturating_add(other.inodes) }
    }
}

fn tree_room(path: &Path) -> std::io::Result<Room> {
    let meta = std::fs::metadata(path)?;
    if !meta.is_dir() {
        return Ok(Room::default().plus_file(meta.len()));
    }
    let mut room = Room { kib: BLOCK / 1024, inodes: 1 };
    for entry in std::fs::read_dir(path)? {
        let entry = entry?;
        let entry_room = if entry.file_type()?.is_dir() {
            tree_room(&entry.path())?
        } else {
            Room::default().plus_file(entry.metadata()?.len())
        };
        room = room.plus(entry_room);
    }
    Ok(room)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::File;
    use std::os::fd::AsRawFd;

    #[test]
    fn a_file_takes_its_blocks_rounded_up_plus_one() {
        assert_eq!(Room::default().plus_file(0), Room { kib: 4, inodes: 1 });
        assert_eq!(Room::default().plus_file(4096), Room { kib: 8, inodes: 1 });
        assert_eq!(Room::default().plus_file(4097), Room { kib: 12, inodes: 1 });
    }

    #[test]
    fn a_tree_counts_every_file_and_directory_and_never_less_than_its_bytes() {
        let dir = tempfile::tempdir().unwrap();
        let sub = dir.path().join("sub");
        std::fs::create_dir(&sub).unwrap();
        std::fs::write(dir.path().join("a"), vec![b'x'; 10_000]).unwrap();
        File::create(sub.join("big")).unwrap().set_len(150 << 20).unwrap();
        let room = Room::for_files(&[dir.path()]).unwrap();
        assert_eq!(room.inodes, 4, "the root, `sub` and two files");
        assert!(room.kib >= (150 << 10) + 10, "{room:?}");
        assert!(room.kib <= (150 << 10) + 64, "{room:?}");
    }

    #[test]
    fn a_link_given_is_followed_and_paths_add_up() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("input");
        std::fs::write(&file, vec![b'x'; 1 << 20]).unwrap();
        let opened = File::open(&file).unwrap();
        let handle = format!("/proc/self/fd/{}", opened.as_raw_fd());
        let one = Room::for_files(&[&file]).unwrap();
        assert_eq!(Room::for_files(&[Path::new(&handle)]).unwrap(), one);
        assert_eq!(Room::for_files(&[&file, &file]).unwrap(), one.plus(one));
    }
}
