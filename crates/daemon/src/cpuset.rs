//! Pins every box to its worker's core with isolate's cgroup cpuset (`box<id>.cpus` in isolate's config).
//! taskset alone can be undone from inside a box with sched_setaffinity; a cpuset cannot.
//! The daemon cannot write isolate's config (root's), so `qhhoj-judge isolate-cpus` prints the lines
//! and the deployer adds them (DEPLOY.md).

use judge_sandbox::boxes::box_pair;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// How the owned boxes are kept on their cores.
#[derive(Debug, PartialEq, Eq)]
pub enum Pinning {
    /// Every owned box has `box<id>.cpus` = its core and isolate's cgroup has the cpuset controller.
    Cpuset,
    /// These boxes have no `cpus` line: taskset only.
    TasksetOnly(Vec<u32>),
    /// isolate would fail or misplace runs; the message says why.
    Broken(String),
}

/// Each owned box with its worker's core: both boxes of worker i on `cores[i]`.
fn box_cores(box_base: u32, cores: &[u32]) -> Vec<(u32, u32)> {
    let pairs = cores
        .iter()
        .enumerate()
        .filter_map(|(worker, &core)| Some((box_pair(box_base, worker as u32)?, core)));
    pairs.flat_map(|(pair, core)| [(pair.solution, core), (pair.helper, core)]).collect()
}

/// The isolate config lines that pin both boxes of every worker to the worker's core.
pub fn isolate_cpus_lines(box_base: u32, cores: &[u32]) -> String {
    box_cores(box_base, cores).iter().map(|(id, core)| format!("box{id}.cpus = {core}\n")).collect()
}

/// `key = value` settings of an isolate config, comments and blank lines skipped.
fn settings(isolate_config: &str) -> impl Iterator<Item = (&str, &str)> {
    isolate_config
        .lines()
        .map(str::trim)
        .filter(|line| !line.starts_with('#'))
        .filter_map(|line| line.split_once('='))
        .map(|(key, value)| (key.trim(), value.trim()))
}

fn box_cpus(isolate_config: &str) -> BTreeMap<u32, &str> {
    let cpus = |key: &str| key.strip_prefix("box")?.strip_suffix(".cpus")?.parse().ok();
    settings(isolate_config).filter_map(|(key, value)| Some((cpus(key)?, value))).collect()
}

/// The pinning of the owned boxes, from isolate's config and its cgroup parent's `cgroup.subtree_control`.
pub fn pinning(isolate_config: &str, subtree_control: &str, box_base: u32, cores: &[u32]) -> Pinning {
    let configured = box_cpus(isolate_config);
    let owned = box_cores(box_base, cores);
    let mut unpinned = Vec::new();
    for &(id, core) in &owned {
        match configured.get(&id) {
            None => unpinned.push(id),
            Some(cpus) if *cpus != core.to_string() => {
                return Pinning::Broken(format!(
                    "isolate config pins box {id} to `{cpus}`, its worker's core is {core}: \
                     regenerate the lines with `qhhoj-judge isolate-cpus`"
                ));
            }
            Some(_) => {}
        }
    }
    let lines_used = unpinned.len() < owned.len();
    if lines_used && !subtree_control.split_whitespace().any(|c| c == "cpuset") {
        return Pinning::Broken(
            "isolate config has box cpus lines but isolate's cgroup has no cpuset controller".into(),
        );
    }
    if unpinned.is_empty() { Pinning::Cpuset } else { Pinning::TasksetOnly(unpinned) }
}

/// isolate's cgroup parent: `cg_root`, or for `auto:FILE` the path isolate's cg keeper wrote to FILE.
fn cgroup_parent(isolate_config: &str) -> Option<PathBuf> {
    let cg_root = settings(isolate_config).find(|(key, _)| *key == "cg_root")?.1;
    match cg_root.strip_prefix("auto:") {
        Some(file) => Some(std::fs::read_to_string(file).ok()?.lines().next()?.trim().into()),
        None => Some(cg_root.into()),
    }
}

/// `pinning` of this host: reads isolate's config at `isolate_config` and its cgroup parent.
pub fn host_pinning(isolate_config: &Path, box_base: u32, cores: &[u32]) -> Pinning {
    let config = std::fs::read_to_string(isolate_config).unwrap_or_default();
    let controllers = cgroup_parent(&config)
        .and_then(|cg| std::fs::read_to_string(cg.join("cgroup.subtree_control")).ok())
        .unwrap_or_default();
    pinning(&config, &controllers, box_base, cores)
}

#[cfg(test)]
mod tests {
    use super::*;

    const ISOLATE_CONFIG: &str = "box_root = /var/local/lib/isolate\ncg_root = auto:/run/isolate/cgroup\n";

    #[test]
    fn the_lines_put_both_boxes_of_each_worker_on_its_core_and_every_owned_box_once() {
        let lines = isolate_cpus_lines(10, &[1, 2, 3]);
        assert_eq!(
            lines,
            "box10.cpus = 1\nbox11.cpus = 1\nbox12.cpus = 2\nbox13.cpus = 2\nbox14.cpus = 3\nbox15.cpus = 3\n"
        );
        let config = format!("{ISOLATE_CONFIG}{lines}");
        assert_eq!(pinning(&config, "cpuset memory", 10, &[1, 2, 3]), Pinning::Cpuset);
    }

    #[test]
    fn boxes_without_lines_are_taskset_only_and_commented_lines_do_not_count() {
        let config = format!("{ISOLATE_CONFIG}box0.cpus = 1\n#box1.cpus = 1\n");
        assert_eq!(pinning(&config, "cpuset memory", 0, &[1, 2]), Pinning::TasksetOnly(vec![1, 2, 3]));
        assert_eq!(pinning(ISOLATE_CONFIG, "memory pids", 0, &[1]), Pinning::TasksetOnly(vec![0, 1]));
    }

    #[test]
    fn lines_without_the_cpuset_controller_or_on_the_wrong_core_are_refused() {
        let config = format!("{ISOLATE_CONFIG}{}", isolate_cpus_lines(0, &[1]));
        assert!(matches!(pinning(&config, "memory pids", 0, &[1]), Pinning::Broken(_)));
        assert!(matches!(pinning(&config, "cpuset memory", 0, &[2]), Pinning::Broken(_)));
    }

    #[test]
    fn the_host_check_follows_cg_root_auto_to_the_cgroup_isolate_uses() {
        let dir = tempfile::tempdir().unwrap();
        let cgroup = dir.path().join("isolate.service");
        std::fs::create_dir(&cgroup).unwrap();
        std::fs::write(cgroup.join("cgroup.subtree_control"), "cpuset memory\n").unwrap();
        std::fs::write(dir.path().join("cgroup"), format!("{}\n", cgroup.display())).unwrap();
        let config = dir.path().join("isolate");
        let auto = format!(
            "cg_root = auto:{}\n{}",
            dir.path().join("cgroup").display(),
            isolate_cpus_lines(0, &[1])
        );
        std::fs::write(&config, auto).unwrap();
        assert_eq!(host_pinning(&config, 0, &[1]), Pinning::Cpuset);
        assert_eq!(host_pinning(&dir.path().join("missing"), 0, &[1]), Pinning::TasksetOnly(vec![0, 1]));
    }
}
