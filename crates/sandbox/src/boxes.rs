//! Worker i owns boxes base+2i (solution) and base+2i+1 (checker, interactor, helper).

use crate::error::SandboxError;
use crate::isolate_box::{SandboxConfig, cleanup_box};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BoxPair {
    pub solution: u32,
    pub helper: u32,
}

/// `None` when the ids would overflow `u32`.
pub fn box_pair(box_base: u32, worker: u32) -> Option<BoxPair> {
    let solution = worker.checked_mul(2)?.checked_add(box_base)?;
    Some(BoxPair { solution, helper: solution.checked_add(1)? })
}

/// Empty when the ids would overflow `u32`.
pub fn owned_box_ids(box_base: u32, slots: u32) -> Vec<u32> {
    let end = slots.checked_mul(2).and_then(|n| n.checked_add(box_base));
    end.map(|end| (box_base..end).collect()).unwrap_or_default()
}

pub fn cleanup_owned(config: &SandboxConfig, box_base: u32, slots: u32) -> Vec<(u32, SandboxError)> {
    owned_box_ids(box_base, slots)
        .into_iter()
        .filter_map(|id| cleanup_box(config, id).err().map(|e| (id, e)))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn workers_get_disjoint_consecutive_pairs() {
        assert_eq!(box_pair(0, 0), Some(BoxPair { solution: 0, helper: 1 }));
        assert_eq!(box_pair(500, 3), Some(BoxPair { solution: 506, helper: 507 }));
        assert_eq!(box_pair(u32::MAX - 1, 1), None);
        assert!(owned_box_ids(u32::MAX - 1, 2).is_empty());
        let ids = owned_box_ids(10, 3);
        assert_eq!(ids, [10, 11, 12, 13, 14, 15]);
        let mut unique = ids.clone();
        unique.dedup();
        assert_eq!(unique.len(), ids.len());
    }

    #[test]
    fn cleanup_reports_every_failure_and_keeps_going() {
        let config = SandboxConfig { isolate_bin: "/nonexistent/isolate".into(), ..SandboxConfig::default() };
        let failures = cleanup_owned(&config, 520, 2);
        assert_eq!(failures.iter().map(|(id, _)| *id).collect::<Vec<_>>(), [520, 521, 522, 523]);
    }
}
