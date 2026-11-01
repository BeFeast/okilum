//! Small FIFO for the production retained local-run classifier.
//! Hash lookup also compares exact bytes; hashes never authorize reuse alone.
use std::{collections::HashMap, collections::VecDeque, sync::Arc};

const MAX_ENTRIES: usize = 16;
const MAX_BYTES: usize = 4096;

pub(super) struct Cache<T> {
    entries: HashMap<Arc<str>, Arc<T>>,
    order: VecDeque<Arc<str>>,
}

impl<T> Default for Cache<T> {
    fn default() -> Self {
        Self {
            entries: HashMap::new(),
            order: VecDeque::new(),
        }
    }
}

impl<T> Cache<T> {
    pub fn get(&self, source: &str) -> Option<Arc<T>> {
        self.entries.get(source).cloned()
    }

    pub fn insert(&mut self, source: &str, value: Arc<T>) {
        if source.len() > MAX_BYTES {
            return;
        }
        if let Some(entry) = self.entries.get_mut(source) {
            *entry = value;
            return;
        }
        if self.entries.len() == MAX_ENTRIES {
            if let Some(oldest) = self.order.pop_front() {
                self.entries.remove(&oldest);
            }
        }
        let source: Arc<str> = source.into();
        self.order.push_back(source.clone());
        self.entries.insert(source, value);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repeated_key_does_not_evict_other_entries() {
        let mut cache = Cache::default();
        for i in 0..MAX_ENTRIES {
            cache.insert(&i.to_string(), Arc::new(i));
        }
        for _ in 0..MAX_ENTRIES * 2 {
            cache.insert("1", Arc::new(100));
        }
        assert_eq!(*cache.get("0").unwrap(), 0);
        assert_eq!(*cache.get("1").unwrap(), 100);
        cache.insert("new", Arc::new(200));
        assert!(cache.get("0").is_none());
        assert_eq!(*cache.get("new").unwrap(), 200);
        assert_eq!(cache.entries.len(), MAX_ENTRIES);
        assert_eq!(cache.order.len(), MAX_ENTRIES);
    }

    #[test]
    fn exact_utf8_keys_and_bounded_admission() {
        let mut cache = Cache::default();
        cache.insert("é", Arc::new(1));
        assert!(cache.get("e\u{301}").is_none());
        assert_eq!(*cache.get("é").unwrap(), 1);
        let boundary = "x".repeat(MAX_BYTES);
        cache.insert(&boundary, Arc::new(2));
        assert_eq!(*cache.get(&boundary).unwrap(), 2);
        let oversized = boundary + "x";
        cache.insert(&oversized, Arc::new(3));
        assert!(cache.get(&oversized).is_none());
    }
}
