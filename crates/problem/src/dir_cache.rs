//! A size-capped LRU cache of directories, one per key. Used for extracted archives and compiled artefacts.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
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
    building: HashSet<String>,
}

type Shared = Arc<(Mutex<State>, Condvar)>;

fn lock(shared: &Shared) -> MutexGuard<'_, State> {
    shared.0.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

pub struct DirCache {
    root: PathBuf,
    max_bytes: u64,
    state: Shared,
    hits: std::sync::atomic::AtomicU64,
}

pub struct CacheLease {
    key: String,
    path: PathBuf,
    state: Shared,
}

impl std::fmt::Debug for CacheLease {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CacheLease").field("key", &self.key).field("path", &self.path).finish()
    }
}

impl CacheLease {
    pub fn key(&self) -> &str {
        &self.key
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for CacheLease {
    fn drop(&mut self) {
        if let Some(entry) = lock(&self.state).entries.get_mut(&self.key) {
            entry.leases -= 1;
        }
    }
}

/// Marks a key as building and clears the mark (waking waiters) on every exit, panics included.
struct BuildGuard<'a> {
    state: &'a Shared,
    key: &'a str,
}

impl Drop for BuildGuard<'_> {
    fn drop(&mut self) {
        lock(self.state).building.remove(self.key);
        self.state.1.notify_all();
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
        if let Some(lease) = self.lease_or_claim(key) {
            self.hits.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            return Ok(lease);
        }
        let _building = BuildGuard { state: &self.state, key };
        let bytes = self.build_entry(key, build)?;
        // The new entry is born leased, so the eviction below can never take it.
        let entry = Entry { bytes, last_used: Instant::now(), leases: 1 };
        lock(&self.state).entries.insert(key.to_string(), entry);
        let lease = CacheLease { key: key.into(), path: self.root.join(key), state: self.state.clone() };
        self.evict();
        Ok(lease)
    }

    /// Drops an entry nobody holds (its data turned out stale); a leased entry stays until its holders
    /// find the same fault. Returns whether the entry is gone.
    pub fn invalidate(&self, key: &str) -> bool {
        let mut state = lock(&self.state);
        if state.entries.get(key).is_some_and(|entry| entry.leases > 0) {
            return false;
        }
        state.entries.remove(key);
        drop(state);
        let _ = std::fs::remove_dir_all(self.root.join(key));
        true
    }

    /// Leases a built entry; when the key is absent, marks it as building and returns None.
    fn lease_or_claim(&self, key: &str) -> Option<CacheLease> {
        let mut state = lock(&self.state);
        while state.building.contains(key) {
            state = self.state.1.wait(state).unwrap_or_else(|poisoned| poisoned.into_inner());
        }
        let Some(entry) = state.entries.get_mut(key) else {
            state.building.insert(key.to_string());
            return None;
        };
        entry.leases += 1;
        entry.last_used = Instant::now();
        Some(CacheLease { key: key.into(), path: self.root.join(key), state: self.state.clone() })
    }

    /// Fills a temp directory and renames it to the key; returns the entry's size.
    fn build_entry<E, F>(&self, key: &str, build: F) -> Result<u64, E>
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
        Ok(dir_size(&target))
    }

    /// Unmaps least recently used free entries while over budget. Their files go after the lock is released;
    /// the keys stay marked as building until then, so a rebuild cannot race the delete.
    fn evict(&self) {
        let mut victims = Vec::new();
        {
            let mut state = lock(&self.state);
            let mut total: u64 = state.entries.values().map(|e| e.bytes).sum();
            while total > self.max_bytes {
                let free = state.entries.iter().filter(|(_, e)| e.leases == 0);
                let Some(key) = free.min_by_key(|(_, e)| e.last_used).map(|(k, _)| k.clone()) else { break };
                if let Some(entry) = state.entries.remove(&key) {
                    total -= entry.bytes;
                }
                state.building.insert(key.clone());
                victims.push(key);
            }
        }
        for key in victims {
            let _ = std::fs::remove_dir_all(self.root.join(&key));
            drop(BuildGuard { state: &self.state, key: &key });
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

    /// Runs `work` on a thread and fails the test when it does not finish (the old bug was a panic, then a deadlock).
    fn within_five_seconds<T: Send + 'static>(work: impl FnOnce() -> T + Send + 'static) -> T {
        let (sender, receiver) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let _ = sender.send(work());
        });
        receiver.recv_timeout(std::time::Duration::from_secs(5)).expect("cache call hung or panicked")
    }

    #[test]
    fn a_new_entry_over_budget_is_leased_not_evicted() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().to_path_buf();
        let (kept, again) = within_five_seconds(move || {
            let cache = DirCache::new(path, 250);
            let (a, b) =
                (cache.get_or_build("a", fill(100)).unwrap(), cache.get_or_build("b", fill(100)).unwrap());
            let third = cache.get_or_build("c", fill(100)).unwrap();
            let kept = third.path().join("f").exists() && a.path().exists() && b.path().exists();
            drop(third);
            let again = cache.get_or_build("c", fill(100)).is_ok();
            (kept, again)
        });
        assert!(kept, "all three leased entries stay on disk");
        assert!(again, "the key is usable afterwards");
    }

    #[test]
    fn a_panicking_build_does_not_block_the_key() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().to_path_buf();
        let rebuilt = within_five_seconds(move || {
            let cache = Arc::new(DirCache::new(path, 1 << 20));
            let crashing = cache.clone();
            let crash = std::thread::spawn(move || {
                let _ = crashing.get_or_build::<std::io::Error, _>("k", |_| panic!("builder bug"));
            });
            assert!(crash.join().is_err());
            cache.get_or_build("k", fill(1)).is_ok()
        });
        assert!(rebuilt);
    }

    #[test]
    fn invalidate_removes_free_entries_and_spares_leased_ones() {
        let root = tempfile::tempdir().unwrap();
        let cache = DirCache::new(root.path().into(), 1 << 20);
        let held = cache.get_or_build("held", fill(1)).unwrap();
        assert!(!cache.invalidate("held"));
        assert!(held.path().exists());
        drop(cache.get_or_build("free", fill(1)).unwrap());
        assert!(cache.invalidate("free"));
        assert!(!root.path().join("free").exists());
        let rebuilds = AtomicU32::new(0);
        drop(
            cache
                .get_or_build("free", |d| {
                    rebuilds.fetch_add(1, Ordering::SeqCst);
                    fill(1)(d)
                })
                .unwrap(),
        );
        assert_eq!(rebuilds.load(Ordering::SeqCst), 1);
    }
}
