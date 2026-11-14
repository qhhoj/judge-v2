//! The built-in checkers run in the daemon, so their heap is bounded by the files they compare: the output and
//! the answer read once each, plus a little, however many lines or tokens the output holds.

use judge_grader::checkers::CheckInput;
use judge_grader::checkers::select::{CheckerChoice, choose};
use judge_problem::init::CheckerSpec;
use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering};

/// The system allocator, counting the bytes in use and their peak since the last `reset_peak`.
struct Counting;

static IN_USE: AtomicUsize = AtomicUsize::new(0);
static PEAK: AtomicUsize = AtomicUsize::new(0);

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let now = IN_USE.fetch_add(layout.size(), Ordering::SeqCst) + layout.size();
        PEAK.fetch_max(now, Ordering::SeqCst);
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        IN_USE.fetch_sub(layout.size(), Ordering::SeqCst);
        unsafe { System.dealloc(ptr, layout) }
    }
}

#[global_allocator]
static ALLOCATOR: Counting = Counting;

/// Heap bytes the check added at its peak.
fn peak_heap(checker: &str, output: &[u8], answer: &[u8]) -> usize {
    let dir = tempfile::tempdir().unwrap();
    let (input, out, ans) = (dir.path().join("in"), dir.path().join("out"), dir.path().join("ans"));
    std::fs::write(&input, b"").unwrap();
    std::fs::write(&out, output).unwrap();
    std::fs::write(&ans, answer).unwrap();
    let CheckerChoice::Builtin(check) = choose(&CheckerSpec::Name(checker.into()), false).unwrap() else {
        panic!("{checker} is not built in")
    };
    let io = CheckInput { input: &input, output: &out, answer: &ans, points: 1.0 };
    let before = IN_USE.load(Ordering::SeqCst);
    PEAK.store(before, Ordering::SeqCst);
    let outcome = check.check(&io).unwrap();
    assert!(!outcome.passed, "{checker}: the output is not the answer");
    PEAK.load(Ordering::SeqCst) - before
}

#[test]
fn checker_heap_stays_within_the_files() {
    const MIB: usize = 1 << 20;
    let answer = b"1\n";
    let mut over = vec![];
    for (checker, unit) in [
        ("sorted", &b"a\n"[..]),
        ("sorted", b"\n"),
        ("unordered", b"a "),
        ("rstripped", b"a\n"),
        ("rstripped", b"\n"),
        ("linematches", b"a\n"),
    ] {
        let output = unit.repeat(2 * MIB / unit.len());
        let peak = peak_heap(checker, &output, answer);
        let bound = output.len() + answer.len() + 64 * 1024;
        if peak > bound {
            over.push(format!("{checker} on {unit:?}: {peak} bytes at peak, over {bound}"));
        }
    }
    assert!(over.is_empty(), "{over:#?}");
}
