//! Short-lived positive caches: a key's value is served for at most `ttl`
//! after it was stored. Negative results are never stored.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

pub struct TtlCache<V> {
    entries: Mutex<HashMap<String, (V, Instant)>>,
    ttl: Duration,
    capacity: usize,
}

impl<V: Clone> TtlCache<V> {
    pub fn new(ttl: Duration, capacity: usize) -> Self {
        Self {
            entries: Mutex::new(HashMap::new()),
            ttl,
            capacity,
        }
    }

    pub fn get(&self, key: &str) -> Option<V> {
        let entries = self.entries.lock().ok()?;
        let (value, stored) = entries.get(key)?;
        (stored.elapsed() < self.ttl).then(|| value.clone())
    }

    /// Stores `value`; a full cache first drops expired entries, then
    /// everything.
    pub fn put(&self, key: &str, value: V) {
        let Ok(mut entries) = self.entries.lock() else {
            return;
        };
        if entries.len() >= self.capacity {
            entries.retain(|_, (_, stored)| stored.elapsed() < self.ttl);
            if entries.len() >= self.capacity {
                entries.clear();
            }
        }
        entries.insert(key.to_owned(), (value, Instant::now()));
    }

    pub fn remove(&self, key: &str) {
        if let Ok(mut entries) = self.entries.lock() {
            entries.remove(key);
        }
    }

    /// Removes every entry whose value matches.
    pub fn remove_where(&self, matches: impl Fn(&V) -> bool) {
        if let Ok(mut entries) = self.entries.lock() {
            entries.retain(|_, (value, _)| !matches(value));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn values_are_served_for_their_ttl_only_and_the_size_is_bounded() {
        let cache = TtlCache::new(Duration::from_millis(50), 2);
        cache.put("a", 1);
        assert_eq!(cache.get("a"), Some(1));
        assert_eq!(cache.get("b"), None);
        std::thread::sleep(Duration::from_millis(60));
        assert_eq!(cache.get("a"), None);
        cache.put("a", 1);
        cache.remove("a");
        assert_eq!(cache.get("a"), None);
        for (key, value) in [("a", 1), ("b", 2), ("c", 3)] {
            cache.put(key, value);
        }
        assert_eq!(cache.get("c"), Some(3));
        assert!(cache.entries.lock().unwrap().len() <= 2);
        cache.remove_where(|value| *value == 3);
        assert_eq!(cache.get("c"), None);
    }
}
