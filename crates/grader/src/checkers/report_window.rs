//! A helper's stdout or stderr, read back through a window of at most `REPORT_WINDOW` bytes at the end of the
//! stream where its protocol puts the verdict (docs/grading.md#checkers). A stream longer than the window keeps
//! only whole lines and is marked `cut`, so no verdict is read from part of a line and a rule that needs the
//! whole stream can refuse it.

use judge_sandbox::IsolateBox;
use std::io::{Read, Seek, SeekFrom};

/// How much of one helper stream is read back.
pub const REPORT_WINDOW: u64 = 64 << 10;

/// The end of the stream the window is kept at.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum End {
    Head,
    Tail,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Report {
    /// The whole stream, or when `cut`, the whole lines inside the window.
    pub text: String,
    /// The stream was longer than the window.
    pub cut: bool,
}

/// The box file `name` through a window at `end`. A missing or unreadable file is an empty stream.
pub fn read_report(sandbox: &IsolateBox, name: &str, end: End) -> Report {
    let Ok(mut file) = sandbox.open_output(name) else { return Report::default() };
    let read = file.metadata().and_then(|meta| read_window(&mut file, meta.len(), end, REPORT_WINDOW));
    read.unwrap_or_default()
}

fn read_window(stream: &mut (impl Read + Seek), len: u64, end: End, window: u64) -> std::io::Result<Report> {
    let mut bytes = Vec::new();
    if len <= window {
        stream.take(window).read_to_end(&mut bytes)?;
        return Ok(Report { text: String::from_utf8_lossy(&bytes).into_owned(), cut: false });
    }
    // One byte more than the window, past its far edge: it tells whether the edge falls between two lines.
    let start = match end {
        End::Head => 0,
        End::Tail => len - window - 1,
    };
    stream.seek(SeekFrom::Start(start))?;
    stream.take(window + 1).read_to_end(&mut bytes)?;
    Ok(Report { text: String::from_utf8_lossy(whole_lines(&bytes, end)).into_owned(), cut: true })
}

/// `bytes` without the line the window's far edge cuts: a head keeps up to its last newline, a tail starts
/// after its first.
fn whole_lines(bytes: &[u8], end: End) -> &[u8] {
    let newline = |b: &u8| *b == b'\n';
    match end {
        End::Head => bytes.iter().rposition(newline).map_or(&[], |at| &bytes[..=at]),
        End::Tail => bytes.iter().position(newline).map_or(&[], |at| &bytes[at + 1..]),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn window_of(stream: &str, end: End) -> Report {
        read_window(&mut Cursor::new(stream.as_bytes()), stream.len() as u64, end, REPORT_WINDOW).unwrap()
    }

    fn lines(count: usize) -> String {
        (0..count).map(|i| format!("{i:07}\n")).collect()
    }

    #[test]
    fn a_stream_inside_the_window_is_read_whole_from_either_end() {
        let stream = format!("{}ok fine\n", lines(100));
        for end in [End::Head, End::Tail] {
            assert_eq!(window_of(&stream, end), Report { text: stream.clone(), cut: false });
        }
        let exact = "x".repeat(REPORT_WINDOW as usize);
        assert!(!window_of(&exact, End::Tail).cut, "a stream of exactly the window is whole");
    }

    #[test]
    fn a_long_stream_keeps_the_whole_lines_at_its_end() {
        let stream = format!("{}points 0.1 graded\n", lines(10_000));
        let tail = window_of(&stream, End::Tail);
        assert!(tail.cut && stream.ends_with(&tail.text) && tail.text.len() <= REPORT_WINDOW as usize);
        assert_eq!(tail.text.lines().last(), Some("points 0.1 graded"));
        assert!(tail.text.lines().all(|line| line.len() == 7 || line.starts_with("points")), "no part line");
        let head = window_of(&stream, End::Head);
        assert!(head.cut && stream.starts_with(&head.text) && head.text.ends_with('\n'));
        assert!(head.text.lines().all(|line| line.len() == 7), "no part line");
    }

    #[test]
    fn a_line_that_starts_exactly_at_the_window_edge_is_kept() {
        let last = format!("{}\n", "y".repeat(REPORT_WINDOW as usize - 1));
        let stream = format!("first\n{last}");
        assert_eq!(window_of(&stream, End::Tail).text, last);
    }

    #[test]
    fn a_line_longer_than_the_window_leaves_nothing_to_read() {
        let stream = format!("1\n{}\n", "9".repeat(REPORT_WINDOW as usize + 10));
        let tail = window_of(&stream, End::Tail);
        assert!(tail.cut && tail.text.is_empty(), "{:?}", &tail.text[..tail.text.len().min(20)]);
        let one_line = "5".repeat(REPORT_WINDOW as usize + 10);
        assert!(window_of(&one_line, End::Head).text.is_empty());
    }
}
