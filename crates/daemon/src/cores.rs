//! One worker per physical core, SMT siblings excluded, the first core left to the daemon (docs/operations.md#config).

use crate::config::Config;
use std::collections::BTreeSet;
use std::path::Path;

/// Where the kernel lists the CPUs.
pub const SYSFS_CPU: &str = "/sys/devices/system/cpu";

fn online(cpu_dir: &Path) -> bool {
    std::fs::read_to_string(cpu_dir.join("online")).map_or(true, |s| s.trim() == "1")
}

fn lowest_sibling(list: &str) -> Option<u32> {
    list.trim().split(',').filter_map(|part| part.split('-').next()?.parse().ok()).min()
}

fn is_cpu_dir(name: &str) -> bool {
    name.strip_prefix("cpu").is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
}

pub fn physical_cores(sysfs_cpu: &Path) -> Vec<u32> {
    let Ok(entries) = std::fs::read_dir(sysfs_cpu) else { return vec![0] };
    let mut cores = BTreeSet::new();
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if !is_cpu_dir(&name) || !online(&entry.path()) {
            continue;
        }
        let siblings =
            std::fs::read_to_string(entry.path().join("topology/thread_siblings_list")).unwrap_or_default();
        cores.extend(lowest_sibling(&siblings));
    }
    cores.into_iter().collect()
}

pub fn worker_cores(config: &Config, sysfs_cpu: &Path) -> Vec<u32> {
    let mut cores = config.cores.clone().unwrap_or_else(|| {
        let all = physical_cores(sysfs_cpu);
        if all.len() > 1 { all[1..].to_vec() } else { all }
    });
    if let Some(slots) = config.slots {
        cores.truncate(slots.max(1) as usize);
    }
    if cores.is_empty() {
        cores.push(0);
    }
    cores
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::tests_support::MIN;

    /// cpu0..cpu7 where cpuN and cpuN+4 are SMT siblings; cpu7 offline.
    fn fake_sysfs() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        for cpu in 0..8u32 {
            let topology = dir.path().join(format!("cpu{cpu}/topology"));
            std::fs::create_dir_all(&topology).unwrap();
            let base = cpu % 4;
            std::fs::write(topology.join("thread_siblings_list"), format!("{},{}\n", base, base + 4))
                .unwrap();
        }
        std::fs::write(dir.path().join("cpu7/online"), "0\n").unwrap();
        dir
    }

    #[test]
    fn one_cpu_per_physical_core() {
        assert_eq!(physical_cores(fake_sysfs().path()), [0, 1, 2, 3]);
    }

    #[test]
    fn workers_leave_the_first_core_and_respect_slots() {
        let sysfs = fake_sysfs();
        let mut config = crate::config::parse(MIN).unwrap();
        assert_eq!(worker_cores(&config, sysfs.path()), [1, 2, 3]);
        config.slots = Some(2);
        assert_eq!(worker_cores(&config, sysfs.path()), [1, 2]);
        config.cores = Some(vec![5, 6]);
        assert_eq!(worker_cores(&config, sysfs.path()), [5, 6]);
    }

    #[test]
    fn a_single_core_machine_still_gets_one_worker() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("cpu0/topology")).unwrap();
        std::fs::write(dir.path().join("cpu0/topology/thread_siblings_list"), "0\n").unwrap();
        let config = crate::config::parse(MIN).unwrap();
        assert_eq!(worker_cores(&config, dir.path()), [0]);
    }
}
