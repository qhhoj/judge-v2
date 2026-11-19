//! isolate's `--meta` file: `key:value` lines.

use crate::error::SandboxError;

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Meta {
    pub status: Option<String>,
    pub exit_code: Option<i32>,
    pub exit_signal: Option<i32>,
    pub time_s: f64,
    pub wall_s: f64,
    /// The cgroup's `memory.peak`; 0 when isolate did not report it (no `memory.peak` before kernel 5.19).
    pub cg_mem_kib: u64,
    /// Peak resident set of the largest process; 0 when isolate did not report it.
    pub max_rss_kib: u64,
    pub oom_killed: bool,
    pub message: String,
}

/// Which of isolate's two memory counters a run's memory is (`Meta::memory_kib`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MemoryCount {
    /// The cgroup peak: every page charged to the run. A contestant's run, whose stdout and stderr are pipes
    /// the judge drains (`drain`), so none of its output is page cache in its cgroup; a `file_io` output file
    /// is, and the judge takes it off the peak first (`grader::file_io_memory`).
    CgroupPeak,
    /// The smaller of the two: jury programs and compilers, whose files are not theirs to answer for.
    Smaller,
}

impl Meta {
    /// The run's memory: what `classify` holds to the memory limit, and what a run reports.
    ///
    /// The cgroup peak counts every page charged to the run: the program's anonymous memory, but also
    /// tmpfs, memfd and pipe pages it holds without mapping them, and the page cache of files it wrote or
    /// read in. The peak RSS counts only mapped pages, including mapped files already in page cache that are
    /// charged elsewhere (a runtime's shared objects, ~37 MiB of PyPy's). `count` says which is the run's
    /// (`MemoryCount`). On a timeout it is the smaller of the two whatever `count` (the max-rss of a TO is
    /// isolate's proxy's). An OOM kill reports the cgroup peak that hit the limit. When isolate reported only
    /// one counter, it is that one (`packaging/check-host.sh` fails a host without `memory.peak`).
    pub fn memory_kib(&self, count: MemoryCount) -> u64 {
        let (cgroup, rss) = (self.cg_mem_kib, self.max_rss_kib);
        if cgroup == 0 || rss == 0 {
            return cgroup.max(rss);
        }
        if self.oom_killed {
            return cgroup;
        }
        match count {
            MemoryCount::CgroupPeak if self.status.as_deref() != Some("TO") => cgroup,
            _ => cgroup.min(rss),
        }
    }
}

fn number<T: std::str::FromStr>(key: &str, value: &str) -> Result<T, SandboxError> {
    value.parse().map_err(|_| SandboxError::Meta(format!("{key}:{value}")))
}

/// One `key:value` line of isolate's meta file. Keys read here:
/// - `status`: `TO` timed out (CPU or wall), `SG` killed by a signal, `RE` exited non-zero, `XX` isolate itself failed;
///   absent when the run ended with exit code 0
/// - `exitcode`, `exitsig`: the exit code, or the signal that killed the run
/// - `time`, `time-wall`: CPU and wall seconds
/// - `cg-mem`: peak memory of the whole cgroup; `max-rss`: peak resident set of the largest process
/// - `cg-oom-killed`: `1` when the cgroup's memory limit killed the run
/// - `message`: isolate's text for a non-zero status
///
/// Other keys (context switches, `killed`) are ignored.
fn apply(meta: &mut Meta, key: &str, value: &str) -> Result<(), SandboxError> {
    match key {
        "status" => meta.status = Some(value.to_string()),
        "exitcode" => meta.exit_code = Some(number(key, value)?),
        "exitsig" => meta.exit_signal = Some(number(key, value)?),
        "time" => meta.time_s = number(key, value)?,
        "time-wall" => meta.wall_s = number(key, value)?,
        "cg-mem" => meta.cg_mem_kib = number(key, value)?,
        "max-rss" => meta.max_rss_kib = number(key, value)?,
        "cg-oom-killed" => meta.oom_killed = value == "1",
        "message" => meta.message = value.to_string(),
        _ => {}
    }
    Ok(())
}

pub fn parse_meta(text: &str) -> Result<Meta, SandboxError> {
    let mut meta = Meta::default();
    for line in text.lines().filter(|l| !l.is_empty()) {
        let (key, value) = line.split_once(':').ok_or_else(|| SandboxError::Meta(line.into()))?;
        apply(&mut meta, key, value)?;
    }
    Ok(meta)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_real_isolate_2_6_oom_meta_skipping_unknown_keys_and_refuses_bad_numbers() {
        let meta = parse_meta(
            "time:0.014\ntime-wall:0.014\nmax-rss:14616\ncsw-voluntary:4\ncsw-forced:5\ncg-mem:8192\n\
             cg-oom-killed:1\nexitsig:9\nstatus:SG\nmessage:Caught fatal signal 9\n",
        )
        .unwrap();
        assert_eq!(meta.status.as_deref(), Some("SG"));
        assert_eq!(meta.exit_signal, Some(9));
        assert!(meta.oom_killed);
        assert_eq!(meta.cg_mem_kib, 8192);
        assert!((meta.time_s - 0.014).abs() < 1e-9);
        assert_eq!(meta.message, "Caught fatal signal 9");
        assert!(matches!(parse_meta("time:fast\n"), Err(SandboxError::Meta(_))));
    }

    #[test]
    fn a_contestants_memory_is_every_page_charged_to_it_and_an_oom_shows_the_cgroup_peak() {
        let memory = |text: &str| parse_meta(text).unwrap().memory_kib(MemoryCount::CgroupPeak);
        // 60 MiB of heap and 22 MiB written to /dev/shm and read back with pread (isolate 2.6)
        assert_eq!(memory("max-rss:62968\ncg-mem:85472\nexitcode:0\n"), 85472, "unmapped pages count");
        assert_eq!(memory("max-rss:56196\ncg-mem:19644\n"), 19644, "PyPy's libpypy pages are not its own");
        assert_eq!(memory("max-rss:3000\ncg-mem:65536\ncg-oom-killed:1\n"), 65536);
    }

    #[test]
    fn memory_is_the_smaller_counter_for_jury_programs_or_when_the_run_timed_out() {
        let memory = |text: &str, count| parse_meta(text).unwrap().memory_kib(count);
        assert_eq!(memory("max-rss:62968\ncg-mem:85472\n", MemoryCount::Smaller), 62968);
        let timeout = "max-rss:824\ncg-mem:131072\nkilled:1\nstatus:TO\n";
        assert_eq!(memory(timeout, MemoryCount::CgroupPeak), 824, "the max-rss of a TO is isolate's proxy's");
    }

    #[test]
    fn memory_is_the_one_counter_isolate_reported_when_the_other_is_missing() {
        let memory = |text: &str| parse_meta(text).unwrap().memory_kib(MemoryCount::CgroupPeak);
        assert_eq!(memory("cg-mem:8192\n"), 8192, "no max-rss: the cgroup peak");
        assert_eq!(memory("max-rss:14616\n"), 14616, "no cg-mem (kernel before 5.19): the peak RSS");
        assert_eq!(memory("max-rss:14616\ncg-mem:0\n"), 14616, "cg-mem 0 is no reading");
        assert_eq!(memory("max-rss:0\ncg-mem:8192\n"), 8192, "max-rss 0 is no reading");
        assert_eq!(memory("max-rss:14616\ncg-oom-killed:1\n"), 14616, "an OOM kill without cg-mem");
        assert_eq!(memory("exitcode:0\n"), 0);
    }
}
