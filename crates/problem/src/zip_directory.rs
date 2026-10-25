//! Which central directory an archive's end records name, found the way the backend's reader (yauzl) finds it: the
//! last end record in the file, and the zip64 end record whenever its locator precedes it. The `zip` crate may read
//! another directory: it falls back to an earlier end record when the last one's directory does not parse (an
//! archive stored inside an entry has one), shifts every offset when it sees prepended bytes, and reads the zip64
//! record only when the plain one is saturated. `extract_zip` refuses an archive on which the two would differ, so
//! the judge never grades files the backend did not check (backend-v2#293).

use std::fs::File;
use std::os::unix::fs::FileExt;

const END_RECORD: u32 = 0x0605_4b50;
const ZIP64_LOCATOR: u32 = 0x0706_4b50;
const ZIP64_RECORD: u32 = 0x0606_4b50;
const END_RECORD_BYTES: usize = 22;
const LOCATOR_BYTES: usize = 20;
const ZIP64_RECORD_BYTES: usize = 56;
const MAX_COMMENT_BYTES: u64 = 0xffff;
const SATURATED_COUNT: u64 = 0xffff;
const SATURATED_OFFSET: u64 = 0xffff_ffff;

/// A central directory: where it starts and how many records the end record says it holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Directory {
    pub start: u64,
    pub entries: u64,
}

fn read_at(file: &File, offset: u64, len: usize) -> Option<Vec<u8>> {
    let mut bytes = vec![0; len];
    file.read_exact_at(&mut bytes, offset).ok()?;
    Some(bytes)
}

fn le(bytes: &[u8], at: usize, width: usize) -> u64 {
    bytes[at..at + width].iter().rev().fold(0, |value, byte| value << 8 | u64::from(*byte))
}

/// The last end record signature in the file, and the record's bytes.
fn last_end_record(file: &File, len: u64) -> Option<(u64, Vec<u8>)> {
    let tail_start = len.saturating_sub(END_RECORD_BYTES as u64 + MAX_COMMENT_BYTES);
    let tail = read_at(file, tail_start, usize::try_from(len - tail_start).ok()?)?;
    let last = tail.len().checked_sub(END_RECORD_BYTES)?;
    let at = (0..=last).rev().find(|&at| le(&tail, at, 4) == u64::from(END_RECORD))?;
    Some((tail_start + at as u64, tail[at..at + END_RECORD_BYTES].to_vec()))
}

/// The zip64 end record's directory when a zip64 locator precedes the end record at `end_at`.
fn zip64_directory(file: &File, end_at: u64) -> Result<Option<Directory>, &'static str> {
    let Some(locator_at) = end_at.checked_sub(LOCATOR_BYTES as u64) else { return Ok(None) };
    let locator = read_at(file, locator_at, LOCATOR_BYTES).ok_or("unreadable zip64 locator")?;
    if le(&locator, 0, 4) != u64::from(ZIP64_LOCATOR) {
        return Ok(None);
    }
    let record = read_at(file, le(&locator, 8, 8), ZIP64_RECORD_BYTES)
        .filter(|record| le(record, 0, 4) == u64::from(ZIP64_RECORD))
        .ok_or("no zip64 end record where its locator points")?;
    Ok(Some(Directory { start: le(&record, 48, 8), entries: le(&record, 32, 8) }))
}

/// The directory yauzl reads, or why the end records let the `zip` crate read another one.
pub fn named_directory(file: &File) -> Result<Directory, &'static str> {
    let len = file.metadata().map_err(|_| "unreadable archive")?.len();
    let (end_at, end) = last_end_record(file, len).ok_or("no end record")?;
    let (on_this_disk, total, start) = (le(&end, 8, 2), le(&end, 10, 2), le(&end, 16, 4));
    if on_this_disk != total {
        return Err("the end record's two entry counts disagree");
    }
    let plain = Directory { start, entries: total };
    let Some(zip64) = zip64_directory(file, end_at)? else { return Ok(plain) };
    let saturated = total == SATURATED_COUNT || start == SATURATED_OFFSET;
    if !saturated && zip64 != plain {
        return Err("the end record and the zip64 end record disagree");
    }
    Ok(zip64)
}
