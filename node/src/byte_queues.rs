//! Viewer-side buffered byte queues, drained by a window with the
//! poke-then-pull watcher pattern (claim with `watch`, drain with `poll`,
//! release with `unwatch`). Shared by the terminal and files planes — the
//! video plane keeps its own richer variant.
//!
//! Token `0` is the *eager* queue [`ByteQueues::ensure`] creates the
//! moment a route goes active: bytes that arrive before the window has
//! subscribed (a shell's first prompt, a listing that raced the window
//! boot) are kept, not dropped — and unlike a video frame, a dropped byte
//! never heals.

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicU64, Ordering};

use parking_lot::Mutex;

struct Watcher {
    token: u64,
    queue: VecDeque<Vec<u8>>,
    queued_bytes: usize,
}

pub struct ByteQueues {
    watchers: Mutex<HashMap<String, Watcher>>,
    tokens: AtomicU64,
    /// A window that never drains caps its buffer here; beyond it the
    /// oldest chunks go (these queues carry live sessions, not archives).
    max_queued: usize,
}

impl ByteQueues {
    pub fn new(max_queued: usize) -> Self {
        ByteQueues {
            watchers: Mutex::new(HashMap::new()),
            // 0 is the eager queue's reserved token.
            tokens: AtomicU64::new(1),
            max_queued,
        }
    }

    /// Make sure a buffer exists for `key` *before* any window subscribes
    /// — called when the route goes active. Token 0 marks it adoptable.
    pub fn ensure(&self, key: &str) {
        self.watchers
            .lock()
            .entry(key.to_string())
            .or_insert(Watcher {
                token: 0,
                queue: VecDeque::new(),
                queued_bytes: 0,
            });
    }

    /// A window claims `key`'s bytes. Adopts the eager queue (keeping
    /// anything buffered) or replaces a previous watcher; returns the
    /// token that scopes the matching `unwatch`.
    pub fn watch(&self, key: &str) -> u64 {
        let token = self.tokens.fetch_add(1, Ordering::Relaxed);
        let mut map = self.watchers.lock();
        let w = map.entry(key.to_string()).or_insert(Watcher {
            token: 0,
            queue: VecDeque::new(),
            queued_bytes: 0,
        });
        w.token = token;
        token
    }

    /// Release a watch claim. The token scopes it: a late unwatch from a
    /// closed window can't remove the queue a newer watcher owns.
    /// Idempotent.
    pub fn unwatch(&self, key: &str, token: u64) {
        let mut map = self.watchers.lock();
        if map.get(key).is_some_and(|w| w.token == token) {
            map.remove(key);
        }
    }

    /// Drop `key`'s queue unconditionally (the route ended).
    pub fn remove(&self, key: &str) {
        self.watchers.lock().remove(key);
    }

    /// Drain everything queued for `key` into one length-prefixed buffer
    /// (`[u32 le len][bytes]…`) for a single IPC hop. Empty when there's
    /// nothing (or no such watcher).
    pub fn poll(&self, key: &str) -> Vec<u8> {
        let mut map = self.watchers.lock();
        let Some(w) = map.get_mut(key) else {
            return Vec::new();
        };
        let mut out = Vec::with_capacity(w.queued_bytes + 4 * w.queue.len());
        for chunk in w.queue.drain(..) {
            out.extend_from_slice(&(chunk.len() as u32).to_le_bytes());
            out.extend_from_slice(&chunk);
        }
        w.queued_bytes = 0;
        out
    }

    /// Buffer one inbound chunk for the watching window. Returns `true`
    /// when the queue went empty → non-empty — the caller's cue to poke
    /// the front-end (mirroring `allmystuff://video-ready`).
    pub fn enqueue(&self, key: &str, bytes: Vec<u8>) -> bool {
        let mut map = self.watchers.lock();
        let Some(w) = map.get_mut(key) else {
            tracing::debug!("no watcher for {key} — bytes dropped");
            return false;
        };
        let was_empty = w.queue.is_empty();
        w.queued_bytes += bytes.len();
        w.queue.push_back(bytes);
        while w.queued_bytes > self.max_queued {
            let Some(old) = w.queue.pop_front() else {
                break;
            };
            w.queued_bytes -= old.len();
            tracing::warn!("queue for {key} unread — oldest chunk dropped");
        }
        was_empty
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn framing_is_byte_exact() {
        let q = ByteQueues::new(4 * 1024 * 1024);
        q.ensure("w1");
        assert!(q.enqueue("w1", vec![1, 2, 3]), "empty → non-empty pokes");
        assert!(!q.enqueue("w1", vec![4]), "already non-empty: no poke");
        let buf = q.poll("w1");
        assert_eq!(
            buf,
            vec![3, 0, 0, 0, 1, 2, 3, 1, 0, 0, 0, 4],
            "[u32 le len][bytes] per chunk"
        );
        assert!(q.poll("w1").is_empty(), "drained");
        assert!(q.enqueue("w1", vec![9]), "poke again after a drain");
    }

    #[test]
    fn watch_adopts_the_eager_queue_and_scopes_unwatch() {
        let q = ByteQueues::new(4 * 1024 * 1024);
        q.ensure("w2");
        q.enqueue("w2", b"early prompt".to_vec());
        let token = q.watch("w2");
        assert_eq!(q.poll("w2")[4..], b"early prompt"[..], "buffer kept");

        // A stale token can't remove the live watcher…
        q.unwatch("w2", token + 999);
        assert!(q.enqueue("w2", vec![1]));
        // …the right one can.
        q.unwatch("w2", token);
        assert!(!q.enqueue("w2", vec![2]), "no watcher — dropped");
    }

    #[test]
    fn overflow_drops_oldest_not_newest() {
        let cap = 4 * 1024 * 1024;
        let q = ByteQueues::new(cap);
        q.ensure("w3");
        let chunk = vec![0u8; 1024 * 1024];
        for _ in 0..4 {
            q.enqueue("w3", chunk.clone());
        }
        q.enqueue("w3", b"newest".to_vec());
        let buf = q.poll("w3");
        let tail = &buf[buf.len() - 6..];
        assert_eq!(tail, b"newest");
        assert!(buf.len() <= cap + 6 + 5 * 4);
    }
}
