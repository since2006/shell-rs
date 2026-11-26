//! The clock and the random numbers, taken only here: a test gives fixed
//! ones, and what a session does depends on nothing else.

use std::sync::Arc;
use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};

/// Where the time and the random numbers come from.
pub trait Env: Send + Sync {
    /// Milliseconds since the Unix epoch.
    fn now_ms(&self) -> i64;
    fn random_u64(&self) -> u64;
}

pub type SharedEnv = Arc<dyn Env>;

/// The system's clock and random numbers.
pub struct SystemEnv;

impl Env for SystemEnv {
    fn now_ms(&self) -> i64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |since| since.as_millis() as i64)
    }

    fn random_u64(&self) -> u64 {
        rand::random()
    }
}

/// A clock that moves only when told and numbers that come in order.
pub struct FixedEnv {
    now: AtomicI64,
    next: AtomicU64,
}

impl FixedEnv {
    pub fn new(now_ms: i64) -> Self {
        Self {
            now: AtomicI64::new(now_ms),
            next: AtomicU64::new(1),
        }
    }

    pub fn advance(&self, ms: i64) {
        self.now.fetch_add(ms, Ordering::SeqCst);
    }
}

impl Env for FixedEnv {
    fn now_ms(&self) -> i64 {
        self.now.load(Ordering::SeqCst)
    }

    fn random_u64(&self) -> u64 {
        // Spread over the range, so a jitter drawn from it is not always 0.
        self.next
            .fetch_add(1, Ordering::SeqCst)
            .wrapping_mul(0x9e37_79b9_7f4a_7c15)
    }
}

/// 16 characters of `[a-z0-9]`, for a conversation's public id.
pub fn random_id(env: &dyn Env) -> String {
    const ALPHABET: &[u8] = b"abcdefghijklmnopqrstuvwxyz0123456789";
    let mut id = String::with_capacity(16);
    let mut bits = env.random_u64();
    for index in 0..16 {
        if index == 8 {
            bits = env.random_u64();
        }
        id.push(ALPHABET[(bits % ALPHABET.len() as u64) as usize] as char);
        bits /= ALPHABET.len() as u64;
    }
    id
}

#[cfg(test)]
mod tests {
    use super::*;

    /// As in the sync core: the clock and the random numbers are read in
    /// this file only, or a session would not be the same twice in a test.
    #[test]
    fn the_clock_and_random_numbers_are_read_here_only() {
        let source = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut stack = vec![source];
        while let Some(folder) = stack.pop() {
            for entry in std::fs::read_dir(folder).unwrap() {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    stack.push(path);
                    continue;
                }
                if path.file_name().is_some_and(|name| name == "env.rs") {
                    continue;
                }
                let text = std::fs::read_to_string(&path).unwrap();
                for forbidden in ["SystemTime::now", "Instant::now", "rand::"] {
                    assert!(
                        !text.contains(forbidden),
                        "{} reads {forbidden}",
                        path.display()
                    );
                }
            }
        }
    }

    #[test]
    fn ids_are_sixteen_lowercase_letters_and_digits() {
        let env = FixedEnv::new(0);
        let (one, two) = (random_id(&env), random_id(&env));
        assert_ne!(one, two);
        for id in [one, two] {
            assert_eq!(id.len(), 16);
            assert!(
                id.bytes()
                    .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit())
            );
        }
    }
}
