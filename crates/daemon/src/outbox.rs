//! Per-assignment numbered messages kept until the bridge acks them (spec §3.6, §3.7).

use crate::batcher::MAX_BATCH_CASES;
use judge_grader::judge::Outcome;
use judge_grader::report::CaseReport;
use std::collections::{HashMap, VecDeque};
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use tokio::sync::Notify;

pub const OUTBOX_WATERMARK: usize = 8 << 20;
const MAX_CASES_PER_MESSAGE: usize = 256;
const MAX_LOG_BYTES: usize = 64 << 10;

/// The bridge's `Welcome.limits` the judge stays inside (preflight M1); they can only lower the spec defaults.
#[derive(Debug)]
pub struct WireLimits {
    max_cases: AtomicUsize,
    max_log_bytes: AtomicUsize,
}

impl Default for WireLimits {
    fn default() -> Self {
        WireLimits {
            max_cases: AtomicUsize::new(MAX_CASES_PER_MESSAGE),
            max_log_bytes: AtomicUsize::new(MAX_LOG_BYTES),
        }
    }
}

fn lowered(sent: u32, default: usize) -> usize {
    if sent == 0 { default } else { default.min(sent as usize) }
}

impl WireLimits {
    pub fn update(&self, max_cases_per_message: u32, max_log_bytes: u32) {
        self.max_cases.store(lowered(max_cases_per_message, MAX_CASES_PER_MESSAGE), Ordering::Relaxed);
        self.max_log_bytes.store(lowered(max_log_bytes, MAX_LOG_BYTES), Ordering::Relaxed);
    }

    pub fn batch_cases(&self) -> usize {
        MAX_BATCH_CASES.min(self.max_cases.load(Ordering::Relaxed))
    }

    pub fn clip_log(&self, log: &str) -> String {
        let mut end = log.len().min(self.max_log_bytes.load(Ordering::Relaxed));
        while !log.is_char_boundary(end) {
            end -= 1;
        }
        log[..end].to_string()
    }
}

fn is_aborted_final(body: &Body) -> bool {
    matches!(body, Body::Final { outcome: Outcome::Aborted, .. })
}

fn aborted_final() -> Body {
    Body::Final { outcome: Outcome::Aborted, log: String::new(), data_version_used: 0 }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct AssignmentRef {
    pub submission_id: String,
    pub generation: u32,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Body {
    Accepted,
    Compiled { log: String },
    Started { pretested: bool, case_count: u32 },
    Cases(Vec<CaseReport>),
    Final { outcome: Outcome, log: String, data_version_used: u64 },
}

#[derive(Debug, Clone, PartialEq)]
pub struct Envelope {
    pub seq: u32,
    pub body: Body,
}

fn size_of(body: &Body) -> usize {
    match body {
        Body::Compiled { log } | Body::Final { log, .. } => 64 + log.len(),
        Body::Cases(cases) => cases
            .iter()
            .map(|c| 64 + c.feedback.len() + c.extended_feedback.len() + c.output_prefix.len())
            .sum(),
        _ => 64,
    }
}

#[derive(Debug, Default)]
pub struct Outbox {
    unacked: VecDeque<(Envelope, usize)>,
    last_seq: u32,
    bytes: usize,
    /// Resume `DROP`: only one `Final{ABORTED}` may still go out.
    dropped: bool,
    /// The bridge sent Abort: it acks no Final of this run, whatever its outcome.
    bridge_aborted: bool,
    /// The newest seq the link sent.
    sent_seq: u32,
}

impl Outbox {
    /// True when the newest message is a `Final` (the run is over).
    fn finished(&self) -> bool {
        self.unacked.back().is_some_and(|(e, _)| matches!(e.body, Body::Final { .. }))
    }

    fn ends_aborted(&self) -> bool {
        self.unacked.back().is_some_and(|(e, _)| is_aborted_final(&e.body))
    }

    /// The bridge acks no Final here: a `Final{ABORTED}`, or any Final of a run the bridge aborted.
    fn never_acked(&self) -> bool {
        self.finished() && (self.ends_aborted() || self.bridge_aborted)
    }

    /// The Final went out and no ack will come for it.
    fn settled(&self) -> bool {
        self.never_acked() && self.sent_seq == self.last_seq
    }

    pub fn push(&mut self, body: Body) -> u32 {
        self.last_seq += 1;
        let size = size_of(&body);
        self.bytes += size;
        self.unacked.push_back((Envelope { seq: self.last_seq, body }, size));
        self.last_seq
    }

    /// Cumulative; true when the acked range included `Final`.
    pub fn ack(&mut self, seq: u32) -> bool {
        let mut final_acked = false;
        while self.unacked.front().is_some_and(|(e, _)| e.seq <= seq) {
            let (envelope, size) = self.unacked.pop_front().expect("checked");
            self.bytes -= size;
            final_acked |= matches!(envelope.body, Body::Final { .. });
        }
        final_acked
    }

    pub fn pending_after(&self, seq: u32) -> Vec<Envelope> {
        self.unacked.iter().filter(|(e, _)| e.seq > seq).map(|(e, _)| e.clone()).collect()
    }

    pub fn last_seq(&self) -> u32 {
        self.last_seq
    }

    pub fn bytes(&self) -> usize {
        self.bytes
    }
}

pub struct Outboxes {
    inner: Mutex<HashMap<AssignmentRef, Outbox>>,
    notify: Notify,
    watermark: usize,
    limits: WireLimits,
}

impl Outboxes {
    pub fn new(watermark: usize) -> Self {
        Outboxes { inner: Mutex::default(), notify: Notify::new(), watermark, limits: WireLimits::default() }
    }

    pub fn open(&self, r: &AssignmentRef) {
        self.inner.lock().unwrap().insert(r.clone(), Outbox::default());
    }

    pub fn limits(&self) -> &WireLimits {
        &self.limits
    }

    /// Clips logs to the bridge's limit; a dropped run keeps only its Final, as `Final{ABORTED}`.
    fn prepared(&self, dropped: bool, body: Body) -> Option<Body> {
        match body {
            Body::Final { .. } if dropped => Some(aborted_final()),
            _ if dropped => None,
            Body::Compiled { log } => Some(Body::Compiled { log: self.limits.clip_log(&log) }),
            Body::Final { outcome, log, data_version_used } => {
                Some(Body::Final { outcome, log: self.limits.clip_log(&log), data_version_used })
            }
            other => Some(other),
        }
    }

    pub fn push(&self, r: &AssignmentRef, body: Body) -> Option<u32> {
        let seq = {
            let mut inner = self.inner.lock().unwrap();
            let outbox = inner.get_mut(r)?;
            let body = self.prepared(outbox.dropped, body)?;
            outbox.push(body)
        };
        // One waiter (the link); notify_one keeps a permit if it is not waiting right now.
        self.notify.notify_one();
        Some(seq)
    }

    /// The link sent `seq`: a Final the bridge never acks (see `Outbox::settled`) takes its outbox with it.
    pub fn sent(&self, r: &AssignmentRef, seq: u32) {
        self.update_settled(r, |outbox| outbox.sent_seq = seq);
    }

    /// The bridge aborted `r`: it never acks the run's Final, so the outbox goes once that Final is sent.
    pub fn bridge_aborted(&self, r: &AssignmentRef) {
        self.update_settled(r, |outbox| outbox.bridge_aborted = true);
    }

    fn update_settled(&self, r: &AssignmentRef, change: impl FnOnce(&mut Outbox)) {
        let mut inner = self.inner.lock().unwrap();
        let Some(outbox) = inner.get_mut(r) else { return };
        change(outbox);
        if outbox.settled() {
            inner.remove(r);
        }
    }

    /// Session end: a finished run the bridge never acks (see `Outbox::never_acked`) is forgotten, sent or not.
    pub fn drop_never_acked(&self) {
        self.inner.lock().unwrap().retain(|_, outbox| !outbox.never_acked());
    }

    /// Resume `DROP`: from now on only one `Final{ABORTED}`; a finished run gets it at once.
    pub fn mark_dropped(&self, r: &AssignmentRef) {
        let mut inner = self.inner.lock().unwrap();
        let Some(outbox) = inner.get_mut(r) else { return };
        outbox.dropped = true;
        if outbox.finished() {
            outbox.push(aborted_final());
        }
        drop(inner);
        self.notify.notify_one();
    }

    /// Whether `r` has an outbox (it is running, or its messages wait for an ack).
    pub fn contains(&self, r: &AssignmentRef) -> bool {
        self.inner.lock().unwrap().contains_key(r)
    }

    pub fn last_seq(&self, r: &AssignmentRef) -> u32 {
        self.inner.lock().unwrap().get(r).map_or(0, Outbox::last_seq)
    }

    pub fn ack(&self, r: &AssignmentRef, seq: u32) -> bool {
        let mut inner = self.inner.lock().unwrap();
        let finished = inner.get_mut(r).is_some_and(|o| o.ack(seq));
        if finished {
            inner.remove(r);
        }
        finished
    }

    pub fn remove(&self, r: &AssignmentRef) {
        self.inner.lock().unwrap().remove(r);
    }

    pub fn pending_after(&self, r: &AssignmentRef, seq: u32) -> Vec<Envelope> {
        self.inner.lock().unwrap().get(r).map(|o| o.pending_after(seq)).unwrap_or_default()
    }

    pub fn in_flight(&self) -> Vec<(AssignmentRef, u32)> {
        self.inner.lock().unwrap().iter().map(|(r, o)| (r.clone(), o.last_seq())).collect()
    }

    pub fn refs(&self) -> Vec<AssignmentRef> {
        self.inner.lock().unwrap().keys().cloned().collect()
    }

    pub fn total_bytes(&self) -> usize {
        self.inner.lock().unwrap().values().map(Outbox::bytes).sum()
    }

    pub fn over_watermark(&self) -> bool {
        self.total_bytes() > self.watermark
    }

    pub fn notified(&self) -> tokio::sync::futures::Notified<'_> {
        self.notify.notified()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use judge_grader::judge::Outcome;

    fn r() -> AssignmentRef {
        AssignmentRef { submission_id: "s1".into(), generation: 3 }
    }

    fn final_body() -> Body {
        Body::Final { outcome: Outcome::Graded, log: String::new(), data_version_used: 0 }
    }

    #[test]
    fn seq_starts_at_one_and_acks_are_cumulative() {
        let mut o = Outbox::default();
        assert_eq!(
            [o.push(Body::Accepted), o.push(Body::Compiled { log: "w".into() }), o.push(final_body())],
            [1, 2, 3]
        );
        assert!(!o.ack(2));
        assert_eq!(o.pending_after(0).iter().map(|e| e.seq).collect::<Vec<_>>(), [3]);
        assert!(o.ack(3), "final acked");
        assert_eq!(o.last_seq(), 3);
    }

    #[test]
    fn stale_or_future_acks_do_not_break_the_outbox() {
        let mut o = Outbox::default();
        o.push(Body::Accepted);
        o.ack(1);
        assert!(!o.ack(1), "duplicate ack");
        assert!(!o.ack(99), "an ack beyond the last seq trims what is there and ends nothing");
        assert_eq!(o.push(Body::Accepted), 2);
    }

    #[test]
    fn watermark_counts_bytes_across_assignments() {
        let boxes = Outboxes::new(1000);
        let (a, b) = (r(), AssignmentRef { submission_id: "s2".into(), generation: 1 });
        boxes.open(&a);
        boxes.open(&b);
        boxes.push(&a, Body::Compiled { log: "x".repeat(600) });
        assert!(!boxes.over_watermark());
        boxes.push(&b, Body::Compiled { log: "x".repeat(600) });
        assert!(boxes.over_watermark());
        boxes.ack(&a, 1);
        assert!(!boxes.over_watermark());
    }

    #[test]
    fn in_flight_lists_last_seq_and_unknown_refs_are_ignored() {
        let boxes = Outboxes::new(OUTBOX_WATERMARK);
        boxes.open(&r());
        boxes.push(&r(), Body::Accepted);
        boxes.push(&r(), Body::Accepted);
        assert_eq!(boxes.in_flight(), [(r(), 2)]);
        let ghost = AssignmentRef { submission_id: "nope".into(), generation: 1 };
        assert_eq!(boxes.push(&ghost, Body::Accepted), None);
        assert!(!boxes.ack(&ghost, 1));
    }

    #[test]
    fn final_ack_removes_the_assignment() {
        let boxes = Outboxes::new(OUTBOX_WATERMARK);
        boxes.open(&r());
        boxes.push(&r(), final_body());
        assert!(boxes.ack(&r(), 1));
        assert!(boxes.refs().is_empty());
    }

    fn aborted() -> Body {
        Body::Final { outcome: Outcome::Aborted, log: String::new(), data_version_used: 0 }
    }

    #[test]
    fn an_aborted_final_goes_once_sent_and_other_finals_wait_for_their_ack() {
        let boxes = Outboxes::new(OUTBOX_WATERMARK);
        boxes.open(&r());
        boxes.push(&r(), Body::Accepted);
        boxes.sent(&r(), 1);
        assert_eq!(boxes.refs(), [r()], "Accepted stays until acked");
        boxes.push(&r(), final_body());
        boxes.sent(&r(), 2);
        assert_eq!(boxes.refs(), [r()], "a graded Final stays until acked");
        let other = AssignmentRef { submission_id: "s9".into(), generation: 1 };
        boxes.open(&other);
        boxes.push(&other, aborted());
        boxes.sent(&other, 1);
        assert_eq!(boxes.refs(), [r()], "a sent Final{{ABORTED}} is never acked, so it goes");
    }

    #[test]
    fn unsent_aborted_finals_go_at_session_end() {
        let boxes = Outboxes::new(OUTBOX_WATERMARK);
        boxes.open(&r());
        boxes.push(&r(), aborted());
        let live = AssignmentRef { submission_id: "live".into(), generation: 1 };
        boxes.open(&live);
        boxes.push(&live, Body::Accepted);
        boxes.drop_never_acked();
        assert_eq!(boxes.refs(), [live]);
    }

    #[test]
    fn the_final_of_a_run_the_bridge_aborted_goes_once_sent_or_at_session_end() {
        let boxes = Outboxes::new(OUTBOX_WATERMARK);
        boxes.open(&r());
        boxes.push(&r(), final_body());
        boxes.sent(&r(), 1);
        boxes.bridge_aborted(&r());
        assert!(boxes.refs().is_empty(), "a sent graded Final the bridge aborted is never acked");
        let late = AssignmentRef { submission_id: "late".into(), generation: 1 };
        let kept = AssignmentRef { submission_id: "kept".into(), generation: 1 };
        for x in [&late, &kept] {
            boxes.open(x);
            boxes.push(x, final_body());
        }
        boxes.bridge_aborted(&late);
        assert_eq!(boxes.refs().len(), 2, "unsent: it may still go out in this session");
        boxes.drop_never_acked();
        assert_eq!(boxes.refs(), [kept], "a graded Final the bridge still wants stays for resend");
    }

    #[test]
    fn a_dropped_run_sends_nothing_but_one_final_aborted() {
        let boxes = Outboxes::new(OUTBOX_WATERMARK);
        boxes.open(&r());
        boxes.push(&r(), Body::Accepted);
        boxes.mark_dropped(&r());
        assert_eq!(boxes.push(&r(), Body::Compiled { log: "x".into() }), None);
        assert_eq!(boxes.push(&r(), final_body()), Some(2));
        assert_eq!(
            boxes.pending_after(&r(), 1).iter().map(|e| e.body.clone()).collect::<Vec<_>>(),
            [aborted()]
        );
        let done = AssignmentRef { submission_id: "done".into(), generation: 1 };
        boxes.open(&done);
        boxes.push(&done, Body::Accepted);
        boxes.push(&done, final_body());
        boxes.mark_dropped(&done);
        assert_eq!(boxes.last_seq(&done), 3, "a finished run gets its Final{{ABORTED}} at once");
        assert_eq!(
            boxes.pending_after(&done, 2).iter().map(|e| e.body.clone()).collect::<Vec<_>>(),
            [aborted()]
        );
    }

    #[test]
    fn wire_limits_only_lower_the_defaults_and_clip_logs() {
        let boxes = Outboxes::new(OUTBOX_WATERMARK);
        assert_eq!(boxes.limits().batch_cases(), 64);
        boxes.limits().update(16, 4);
        assert_eq!(boxes.limits().batch_cases(), 16);
        assert_eq!(boxes.limits().clip_log("ééé"), "éé");
        boxes.limits().update(0, 1 << 30);
        assert_eq!(boxes.limits().batch_cases(), 64, "0 means not sent");
        assert_eq!(
            boxes.limits().clip_log(&"x".repeat(70_000)).len(),
            64 << 10,
            "never above the spec's 64 KiB"
        );
        boxes.limits().update(256, 3);
        boxes.open(&r());
        boxes.push(&r(), Body::Compiled { log: "abcdef".into() });
        assert_eq!(boxes.pending_after(&r(), 0)[0].body, Body::Compiled { log: "abc".into() });
    }
}
