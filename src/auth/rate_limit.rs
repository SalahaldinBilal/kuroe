use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::Mutex;
use std::time::{Duration, Instant};

const MAX_FAILURES: u32 = 10;
const WINDOW: Duration = Duration::from_secs(15 * 60);

#[derive(Default)]
pub struct LoginLimiter {
    failures: Mutex<HashMap<IpAddr, (u32, Instant)>>,
}

impl LoginLimiter {
    pub fn is_blocked(&self, ip: IpAddr) -> bool {
        let mut map = self.failures.lock().unwrap();
        match map.get(&ip) {
            Some((_, since)) if since.elapsed() > WINDOW => {
                map.remove(&ip);
                false
            }
            Some((count, _)) => *count >= MAX_FAILURES,
            None => false,
        }
    }

    pub fn record_failure(&self, ip: IpAddr) {
        let mut map = self.failures.lock().unwrap();
        let entry = map.entry(ip).or_insert((0, Instant::now()));
        entry.0 += 1;
    }

    pub fn clear(&self, ip: IpAddr) {
        self.failures.lock().unwrap().remove(&ip);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blocks_after_max_failures() {
        let limiter = LoginLimiter::default();
        let ip: IpAddr = "10.0.0.1".parse().unwrap();
        for _ in 0..MAX_FAILURES {
            assert!(!limiter.is_blocked(ip));
            limiter.record_failure(ip);
        }
        assert!(limiter.is_blocked(ip));
        limiter.clear(ip);
        assert!(!limiter.is_blocked(ip));
    }
}
