//! A manual test written on Windows ends its lines with CRLF; the package Polygon builds for Linux (the
//! "full" package) ends them with LF. Manual inputs are copied the second way so the answers and the
//! validator see the same bytes either package would give.

use std::io::{Read, Write};

const CHUNK_BYTES: usize = 64 << 10;

/// Copies `reader` to `writer` leaving out every CR that is directly followed by LF; returns the bytes written.
pub fn copy_lf(mut reader: impl Read, mut writer: impl Write) -> std::io::Result<u64> {
    let mut chunk = vec![0u8; CHUNK_BYTES];
    let (mut held_cr, mut written) = (false, 0u64);
    loop {
        let read = reader.read(&mut chunk)?;
        if read == 0 {
            break;
        }
        let mut out = Vec::with_capacity(read + 1);
        for &byte in &chunk[..read] {
            if held_cr && byte != b'\n' {
                out.push(b'\r');
            }
            held_cr = byte == b'\r';
            if !held_cr {
                out.push(byte);
            }
        }
        writer.write_all(&out)?;
        written += out.len() as u64;
    }
    if held_cr {
        writer.write_all(b"\r")?;
        written += 1;
    }
    Ok(written)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;

    fn lf(input: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        let written = copy_lf(input, &mut out).unwrap();
        assert_eq!(written, out.len() as u64);
        out
    }

    /// Yields one byte per read, so a CR and its LF are always in different chunks.
    struct Trickle<'a>(&'a [u8]);

    impl Read for Trickle<'_> {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            let Some((&first, rest)) = self.0.split_first() else { return Ok(0) };
            buf[0] = first;
            self.0 = rest;
            Ok(1)
        }
    }

    #[test]
    fn crlf_becomes_lf_and_other_bytes_are_kept() {
        assert_eq!(lf(b"1 6\r\n0.5\r\n"), b"1 6\n0.5\n");
        assert_eq!(lf(b"a\rb\r\r\nc\r"), b"a\rb\r\nc\r", "only a CR directly before LF goes");
        assert_eq!(lf(b"already\nlf"), b"already\nlf");
        assert_eq!(lf(b""), b"");
    }

    #[test]
    fn a_crlf_split_across_reads_is_still_one_line_ending() {
        let mut out = Vec::new();
        copy_lf(Trickle(b"x\r\ny\r\r\nz\r"), &mut out).unwrap();
        assert_eq!(out, b"x\ny\r\nz\r");
    }
}
