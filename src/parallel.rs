//! Small scoped work-sharing helpers (std only, no dependencies).
//!
//! Used for CPU-heavy, independent jobs: screenshot conversion, palette
//! building, template rendering/writing. The compositor's frame loop itself
//! stays on the main thread — GPU work is already asynchronous and a single
//! output cannot be split across cores meaningfully.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;

pub fn threads() -> usize {
    std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1)
}

/// Run `f(0)..f(n)` on up to `threads()` cores. Falls back to a plain loop
/// when there is only one core or one item.
pub fn for_each<F>(n: usize, f: F)
where
    F: Fn(usize) + Sync,
{
    if n == 0 {
        return;
    }
    let workers = threads().min(n);
    if workers <= 1 {
        for i in 0..n {
            f(i);
        }
        return;
    }
    let next = AtomicUsize::new(0);
    let f = &f;
    std::thread::scope(|scope| {
        for _ in 0..workers {
            scope.spawn(|| loop {
                let i = next.fetch_add(1, Ordering::Relaxed);
                if i >= n {
                    break;
                }
                f(i);
            });
        }
    });
}

/// Parallel map: same as `for_each`, but collects the results in input order.
pub fn map<T, F>(n: usize, f: F) -> Vec<T>
where
    T: Send,
    F: Fn(usize) -> T + Sync,
{
    if n == 0 {
        return Vec::new();
    }
    let slots: Mutex<Vec<Option<T>>> = Mutex::new((0..n).map(|_| None).collect());
    for_each(n, |i| {
        let value = f(i);
        slots.lock().unwrap()[i] = Some(value);
    });
    let mut slots = slots.into_inner().unwrap();
    slots.drain(..).map(|v| v.expect("parallel::map slot")).collect()
}
