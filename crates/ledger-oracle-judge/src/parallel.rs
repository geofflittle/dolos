use std::{
    cmp::Reverse,
    panic,
    sync::atomic::{AtomicUsize, Ordering},
    thread,
};

/// The judgement of each item in item order, judged on `threads` threads that
/// take the heaviest waiting item first.
pub fn in_parallel<T: Sync, R: Send>(
    items: &[T],
    threads: usize,
    weight: impl Fn(&T) -> usize,
    judge: impl Fn(&T) -> R + Sync,
) -> Vec<R> {
    let mut order: Vec<usize> = (0..items.len()).collect();
    order.sort_by_key(|i| Reverse(weight(&items[*i])));
    let next = AtomicUsize::new(0);
    let worker = || {
        let mut judged = Vec::new();
        while let Some(&i) = order.get(next.fetch_add(1, Ordering::Relaxed)) {
            judged.push((i, judge(&items[i])));
        }
        judged
    };
    let mut judged: Vec<(usize, R)> = thread::scope(|s| {
        let workers: Vec<_> = (0..threads.max(1)).map(|_| s.spawn(worker)).collect();
        let joined = workers.into_iter().map(|w| w.join());
        let judged = joined.map(|j| j.unwrap_or_else(|payload| panic::resume_unwind(payload)));
        judged.flatten().collect()
    });
    judged.sort_by_key(|(i, _)| *i);
    judged.into_iter().map(|(_, r)| r).collect()
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;

    #[test]
    fn equal_weights_start_in_item_order() {
        let started = Mutex::new(Vec::new());
        let items: Vec<usize> = (0..6).collect();
        in_parallel(&items, 1, |_| 1, |i| started.lock().unwrap().push(*i));
        assert_eq!(started.into_inner().unwrap(), items);
    }

    #[test]
    fn the_heaviest_item_starts_first() {
        let started = Mutex::new(Vec::new());
        let items: Vec<usize> = vec![2, 5, 1, 4];
        in_parallel(&items, 1, |i| *i, |i| started.lock().unwrap().push(*i));
        assert_eq!(started.into_inner().unwrap(), [5, 4, 2, 1]);
    }

    #[test]
    fn judgements_come_back_in_item_order_on_every_thread_count() {
        let items: Vec<usize> = (0..40).collect();
        let doubled: Vec<usize> = items.iter().map(|i| 2 * i).collect();
        for threads in [1, 3, 64] {
            let judged = in_parallel(&items, threads, |i| *i % 7, |i| 2 * i);
            assert_eq!(judged, doubled, "threads={threads}");
        }
    }

    #[test]
    #[should_panic(expected = "item 3 failed")]
    fn a_panicking_item_panics_with_its_own_message() {
        let items: Vec<usize> = (0..8).collect();
        in_parallel(&items, 3, |_| 1, |i| assert_ne!(*i, 3, "item 3 failed"));
    }
}
