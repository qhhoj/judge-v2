//! A size-capped LRU cache of directories, one per key. Used for extracted archives and compiled artefacts.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Instant;

#[derive(Debug)]
struct Entry {
    bytes: u64,
    last_used: Instant,
    leases: u32,
}

#[derive(Default)]
struct State {
    entries: HashMap<String, Entry>,
    building: std::collections::HashSet<String>,
}

pub struct DirCache {
    root: PathBuf,
    max_bytes: u64,
    state: Arc<(Mutex<State>, Condvar)>,
    hits: std::sync::atomic::AtomicU64,
}

pub struct CacheLease {
    key: String,
    path: PathBuf,
    state: Arc<(Mutex<State>, Condvar)>,
}

impl std::fmt::Debug for CacheLease {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CacheLease").field("key", &self.key).field("path", &self.path).finish()
    }
}

impl CacheLease {
    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for CacheLease {
    fn drop(&mut self) {
        let mut state = self.state.0.lock().unwrap();
        if let Some(entry) = state.entries.get_mut(&self.key) {
            entry.leases -= 1;
        }
    }
}

fn dir_size(path: &Path) -> u64 {
    let Ok(entries) = std::fs::read_dir(path) else { return 0 };
    entries
        .flatten()
        .map(|e| match e.metadata() {
            Ok(m) if m.is_dir() => dir_size(&e.path()),
            Ok(m) => m.len(),
            Err(_) => 0,
        })
        .sum()
}

impl DirCache {
    pub fn new(root: PathBuf, max_bytes: u64) -> Self {
        DirCache { root, max_bytes, state: Arc::default(), hits: Default::default() }
    }

    /// Leases served without a build (the `judge_cache_hits_total` metric).
    pub fn hits(&self) -> u64 {
        self.hits.load(std::sync::atomic::Ordering::Relaxed)
    }

    pub fn get_or_build<E, F>(&self, key: &str, build: F) -> Result<CacheLease, E>
    where
        E: From<std::io::Error>,
        F: FnOnce(&Path) -> Result<(), E>,
    {
        if let Some(lease) = self.wait_and_lease(key) {
            self.hits.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            return Ok(lease);
        }
        let result = self.build_entry(key, build);
        let mut state = self.state.0.lock().unwrap();
        state.building.remove(key);
        self.state.1.notify_all();
        drop(state);
        result?;
        self.evict();
        Ok(self.wait_and_lease(key).expect("entry exists right after its build"))
    }

    /// Leases a built entry; marks the key as building and returns None when it is absent.
    fn wait_and_lease(&self, key: &str) -> Option<CacheLease> {
        let mut state = self.state.0.lock().unwrap();
        while state.building.contains(key) {
            state = self.state.1.wait(state).unwrap();
        }
        let Some(entry) = state.entries.get_mut(key) else {
            state.building.insert(key.to_string());
            return None;
        };
        entry.leases += 1;
        entry.last_used = Instant::now();
        Some(CacheLease { key: key.into(), path: self.root.join(key), state: self.state.clone() })
    }

    fn build_entry<E, F>(&self, key: &str, build: F) -> Result<(), E>
    where
        E: From<std::io::Error>,
        F: FnOnce(&Path) -> Result<(), E>,
    {
        let target = self.root.join(key);
        let temp = self.root.join(format!(".build-{}", key.replace('/', "_")));
        let _ = std::fs::remove_dir_all(&temp);
        std::fs::create_dir_all(&temp)?;
        if let Err(error) = build(&temp) {
            let _ = std::fs::remove_dir_all(&temp);
            return Err(error);
        }
        let _ = std::fs::remove_dir_all(&target);
        std::fs::create_dir_all(target.parent().unwrap_or(&self.root))?;
        std::fs::rename(&temp, &target)?;
        let bytes = dir_size(&target);
        let entry = Entry { bytes, last_used: Instant::now(), leases: 0 };
        self.state.0.lock().unwrap().entries.insert(key.to_string(), entry);
        Ok(())
    }

    fn evict(&self) {
        let mut state = self.state.0.lock().unwrap();
        let mut total: u64 = state.entries.values().map(|e| e.bytes).sum();
        while total > self.max_bytes {
            let victim = state.entries.iter().filter(|(_, e)| e.leases == 0).min_by_key(|(_, e)| e.last_used);
            let Some((key, entry)) = victim.map(|(k, e)| (k.clone(), e.bytes)) else { break };
            state.entries.remove(&key);
            let _ = std::fs::remove_dir_all(self.root.join(&key));
            total -= entry;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU32, Ordering};

    fn fill(bytes: usize) -> impl FnOnce(&Path) -> Result<(), std::io::Error> {
        move |dir| std::fs::write(dir.join("f"), vec![0u8; bytes])
    }

    #[test]
    fn builds_once_and_reuses() {
        let root = tempfile::tempdir().unwrap();
        let cache = DirCache::new(root.path().into(), 1 << 20);
        let builds = AtomicU32::new(0);
        for _ in 0..3 {
            let lease = cache
                .get_or_build("k", |dir| {
                    builds.fetch_add(1, Ordering::SeqCst);
                    fill(10)(dir)
                })
                .unwrap();
            assert!(lease.path().join("f").exists());
        }
        assert_eq!(builds.load(Ordering::SeqCst), 1);
        assert_eq!(cache.hits(), 2);
    }

    #[test]
    fn a_failed_build_leaves_nothing_and_retries_next_time() {
        let root = tempfile::tempdir().unwrap();
        let cache = DirCache::new(root.path().into(), 1 << 20);
        let failed = cache.get_or_build("k", |_| Err(std::io::Error::other("boom")));
        assert!(failed.is_err());
        assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 0);
        assert!(cache.get_or_build("k", fill(1)).is_ok());
    }

    #[test]
    fn eviction_drops_least_recent_unleased_entries_only() {
        let root = tempfile::tempdir().unwrap();
        let cache = DirCache::new(root.path().into(), 250);
        let held = cache.get_or_build("a", fill(100)).unwrap();
        drop(cache.get_or_build("b", fill(100)).unwrap());
        drop(cache.get_or_build("c", fill(100)).unwrap());
        assert!(held.path().exists(), "a leased entry is never evicted");
        assert!(!root.path().join("b").exists(), "the oldest free entry goes first");
        assert!(root.path().join("c").exists());
    }

    #[test]
    fn concurrent_callers_build_one_key_once() {
        let root = tempfile::tempdir().unwrap();
        let cache = Arc::new(DirCache::new(root.path().into(), 1 << 20));
        let builds = Arc::new(AtomicU32::new(0));
        let threads: Vec<_> = (0..4)
            .map(|_| {
                let (cache, builds) = (cache.clone(), builds.clone());
                std::thread::spawn(move || {
                    cache
                        .get_or_build("k", |dir| {
                            builds.fetch_add(1, Ordering::SeqCst);
                            std::thread::sleep(std::time::Duration::from_millis(50));
                            fill(1)(dir)
                        })
                        .map(|_| ())
                })
            })
            .collect();
        threads.into_iter().for_each(|t| t.join().unwrap().unwrap());
        assert_eq!(builds.load(Ordering::SeqCst), 1);
    }
}
