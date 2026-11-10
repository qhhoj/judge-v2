//! A run's stdout or stderr as a pipe the judge reads (`IsolateBox::run_drained`). What the program writes
//! there never becomes a file of the box, so no page cache of it is charged to the run's cgroup and the
//! cgroup peak is the program's own memory (`Meta::memory_kib`).
//!
//! A pipe takes the place of a box file capped by `--fsize`: the judge reads at most the file size limit,
//! then closes its end, so the program's next write gets SIGPIPE where a write past the limit got SIGXFSZ.

use crate::error::SandboxError;
use std::io::{PipeReader, PipeWriter, Read};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::time::Duration;

const CHUNK: usize = 64 << 10;
/// After a read that found less than half a chunk, the reader waits this long before the next one, so a
/// program that writes many small pieces (a flush per line) fills the pipe without waking the reader on every
/// write, which would cost the program CPU time; a program that writes faster finds the reader at full speed.
const PACE: Duration = Duration::from_millis(1);

/// Which bytes of a stream the judge keeps: the last `last` of its first `first`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Keep {
    pub first: u64,
    pub last: u64,
}

impl Keep {
    /// The first `bytes` of the stream.
    pub fn first(bytes: u64) -> Keep {
        Keep { first: bytes, last: bytes }
    }
}

/// What the judge read of one stream.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Drained {
    /// The bytes `Keep` asked for.
    pub kept: Vec<u8>,
    /// The stream reached the file size limit and the judge closed its end.
    pub cut: bool,
}

/// A stream being read on its own thread, so a program never waits on the judge for long.
pub struct Drain {
    result: mpsc::Receiver<std::io::Result<Drained>>,
}

impl Drain {
    /// A pipe whose read end is drained from now on; the write end is for the run's command.
    pub fn start(keep: Keep, limit: u64) -> Result<(Drain, PipeWriter), SandboxError> {
        let (reader, writer) = std::io::pipe()?;
        let (send, result) = mpsc::channel();
        std::thread::Builder::new()
            .name("qhhoj-drain".into())
            .spawn(move || send.send(drain(reader, keep, limit)))?;
        Ok((Drain { result }, writer))
    }

    /// The stream once every write end is closed. One still open `grace` after the run ended (a process the
    /// box left behind) is an error; the box's next `--cleanup` kills it and ends the thread.
    pub fn finish(self, grace: Duration) -> Result<Drained, SandboxError> {
        match self.result.recv_timeout(grace) {
            Ok(drained) => Ok(drained?),
            Err(RecvTimeoutError::Timeout) => Err(SandboxError::Isolate("a run's output stayed open".into())),
            Err(RecvTimeoutError::Disconnected) => Err(SandboxError::Io("the output reader stopped".into())),
        }
    }
}

/// Reads until end of stream or `limit` bytes, keeping what `keep` asks for. Returning drops the read end.
fn drain(mut pipe: PipeReader, keep: Keep, limit: u64) -> std::io::Result<Drained> {
    let mut buffer = vec![0u8; CHUNK];
    let (mut drained, mut total) = (Drained::default(), 0u64);
    while total < limit {
        let want = (limit - total).min(CHUNK as u64) as usize;
        let read = match pipe.read(&mut buffer[..want]) {
            Ok(0) => break,
            Ok(read) => read,
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error),
        };
        keep_chunk(&mut drained.kept, &buffer[..read], total, keep);
        total += read as u64;
        if read < CHUNK / 2 {
            std::thread::sleep(PACE);
        }
    }
    drained.cut = total >= limit;
    trim_to(&mut drained.kept, keep.last);
    Ok(drained)
}

/// Adds the part of `chunk` (which starts at stream offset `at`) inside the first `keep.first` bytes, trimming
/// to the last `keep.last` once twice that is held.
fn keep_chunk(kept: &mut Vec<u8>, chunk: &[u8], at: u64, keep: Keep) {
    let inside = keep.first.saturating_sub(at).min(chunk.len() as u64) as usize;
    kept.extend_from_slice(&chunk[..inside]);
    if kept.len() as u64 >= keep.last.saturating_mul(2) {
        trim_to(kept, keep.last);
    }
}

fn trim_to(kept: &mut Vec<u8>, last: u64) {
    let excess = kept.len().saturating_sub(usize::try_from(last).unwrap_or(usize::MAX));
    kept.drain(..excess);
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    const GRACE: Duration = Duration::from_secs(5);

    fn drained(bytes: &[u8], keep: Keep, limit: u64) -> Drained {
        let (drain, mut writer) = Drain::start(keep, limit).unwrap();
        let _ = writer.write_all(bytes);
        drop(writer);
        drain.finish(GRACE).unwrap()
    }

    #[test]
    fn the_first_bytes_are_kept_exactly() {
        let bytes: Vec<u8> = (0..=255u8).cycle().take(200_000).collect();
        let all = drained(&bytes, Keep::first(1 << 20), 1 << 20);
        assert_eq!(all, Drained { kept: bytes.clone(), cut: false });
        let head = drained(&bytes, Keep::first(4097), 1 << 20);
        assert_eq!((head.kept.as_slice(), head.cut), (&bytes[..4097], false));
        assert_eq!(drained(b"", Keep::first(10), 10), Drained::default());
    }

    #[test]
    fn the_tail_of_a_window_is_kept() {
        let bytes: Vec<u8> = (0..1_000_000u32).map(|i| (i % 251) as u8).collect();
        let tail = drained(&bytes, Keep { first: 700_000, last: 1000 }, 1 << 20);
        assert_eq!(tail.kept, &bytes[699_000..700_000]);
        let short = drained(&bytes[..500], Keep { first: 700_000, last: 1000 }, 1 << 20);
        assert_eq!(short.kept, &bytes[..500]);
    }

    #[test]
    fn the_stream_is_cut_at_the_limit_and_the_writer_then_fails() {
        let (drain, mut writer) = Drain::start(Keep::first(10), 100_000).unwrap();
        let block = [7u8; 4096];
        let failed = (0..10_000).find_map(|_| writer.write_all(&block).err());
        assert_eq!(failed.map(|error| error.kind()), Some(std::io::ErrorKind::BrokenPipe));
        drop(writer);
        assert_eq!(drain.finish(GRACE).unwrap(), Drained { kept: vec![7; 10], cut: true });
        assert!(drained(&[1; 100], Keep::first(10), 100).cut, "exactly the limit closes the stream");
        assert!(!drained(&[1; 99], Keep::first(10), 100).cut);
    }

    #[test]
    fn a_stream_left_open_is_an_error_after_the_grace() {
        let (drain, writer) = Drain::start(Keep::first(10), 100).unwrap();
        assert!(drain.finish(Duration::from_millis(50)).is_err());
        drop(writer);
    }
}
