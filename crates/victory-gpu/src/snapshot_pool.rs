//! Reuses completed shadow snapshots to avoid faulting in fresh pages.

#[cfg(windows)]
use std::sync::LazyLock;
use std::sync::Mutex;

const MIB: usize = 1 << 20;
const DEFAULT_BYTES: usize = 512 * MIB;

#[cfg(windows)]
pub(super) static SNAPSHOT_POOL: LazyLock<SnapshotPool> = LazyLock::new(|| {
    let setting = std::env::var("VICTORY_GPU_SNAPSHOT_POOL_MIB").ok();
    SnapshotPool::new(budget_bytes(setting.as_deref()))
});

fn budget_bytes(setting: Option<&str>) -> usize {
    setting
        .and_then(|value| value.parse::<usize>().ok())
        .and_then(|mib| mib.checked_mul(MIB))
        .unwrap_or(DEFAULT_BYTES)
}

pub(super) struct SnapshotPool {
    budget: usize,
    buffers: Mutex<Vec<Vec<u8>>>,
}

impl SnapshotPool {
    fn new(budget: usize) -> Self {
        Self {
            budget,
            buffers: Mutex::new(Vec::new()),
        }
    }

    /// An empty buffer with room for at least `n` bytes.
    pub(super) fn take(&self, n: usize) -> Vec<u8> {
        if self.budget == 0 || n > self.budget {
            return Vec::with_capacity(n);
        }
        let mut buffers = self.buffers.lock().unwrap_or_else(|e| e.into_inner());
        let fit = buffers
            .iter()
            .enumerate()
            .filter(|(_, v)| v.capacity() >= n)
            .min_by_key(|(_, v)| v.capacity())
            .map(|(i, _)| i);
        let reused = fit.map(|i| buffers.swap_remove(i));
        drop(buffers);
        let mut v = reused.unwrap_or_else(|| Vec::with_capacity(n));
        v.clear();
        v
    }

    /// Keeps the largest-first prefix that fits within the aggregate budget.
    pub(super) fn give_back(&self, v: Vec<u8>) {
        if v.capacity() == 0 || v.capacity() > self.budget {
            return;
        }
        let mut buffers = self.buffers.lock().unwrap_or_else(|e| e.into_inner());
        buffers.push(v);
        buffers.sort_by_key(|v| std::cmp::Reverse(v.capacity()));
        let mut remaining = self.budget;
        let keep = buffers
            .iter()
            .take_while(|v| {
                let Some(bytes) = remaining.checked_sub(v.capacity()) else {
                    return false;
                };
                remaining = bytes;
                true
            })
            .count();
        buffers.truncate(keep);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_budget_and_falls_back_to_default() {
        assert_eq!(budget_bytes(Some("128")), 128 * MIB);
        assert_eq!(budget_bytes(Some("0")), 0);
        let overflow = (usize::MAX / MIB + 1).to_string();
        for setting in [
            None,
            Some(""),
            Some("-1"),
            Some("abc"),
            Some(overflow.as_str()),
        ] {
            assert_eq!(budget_bytes(setting), DEFAULT_BYTES);
        }
    }

    #[test]
    fn retains_largest_prefix_and_reuses_the_smallest_fit() {
        let pool = SnapshotPool::new(12);
        pool.give_back(vec![1; 5]);
        pool.give_back(vec![2; 3]);
        pool.give_back(vec![3; 7]);
        let v = pool.take(3);
        assert_eq!(v.capacity(), 5);
        assert!(v.is_empty());
        let v = pool.take(5);
        assert_eq!(v.capacity(), 7);
        assert!(v.is_empty());
        assert!(pool.buffers.lock().unwrap().is_empty());
    }

    #[test]
    fn oversized_return_keeps_reusable_buffers() {
        let pool = SnapshotPool::new(8);
        pool.give_back(vec![1; 8]);
        let mut oversized = vec![2; 9];
        oversized.clear();
        pool.give_back(oversized);
        assert_eq!(pool.take(1).capacity(), 8);
    }

    #[test]
    fn zero_budget_keeps_no_buffers() {
        let pool = SnapshotPool::new(0);
        pool.give_back(vec![1; 8]);
        pool.give_back(Vec::new());
        assert!(pool.buffers.lock().unwrap().is_empty());
        assert!(pool.take(8).capacity() >= 8);
    }
}
