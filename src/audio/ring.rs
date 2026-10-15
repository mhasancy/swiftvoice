//! Bounded, lock-free single-producer/single-consumer ring buffer for audio
//! samples.
//!
//! The producer runs inside the audio driver callback, which must never
//! block or allocate. When the consumer falls behind, new samples are
//! dropped (never the ones already queued) and counted, so memory stays
//! bounded and overruns are visible in the logs instead of silent.

use std::cell::UnsafeCell;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;

struct Shared {
    buf: Box<[UnsafeCell<f32>]>,
    cap: usize,
    /// Total samples ever written (wrapping). Only the producer stores it.
    head: AtomicUsize,
    /// Total samples ever read (wrapping). Only the consumer stores it.
    tail: AtomicUsize,
    dropped: AtomicU64,
}

// SAFETY: the producer only writes slots in [head, tail + cap) and the
// consumer only reads slots in [tail, head); the Release/Acquire pairs on
// head and tail order those accesses, so no slot is accessed concurrently.
unsafe impl Sync for Shared {}
unsafe impl Send for Shared {}

impl Shared {
    fn ptr(&self) -> *mut f32 {
        // UnsafeCell<f32> has the same in-memory representation as f32.
        self.buf.as_ptr() as *mut f32
    }
}

/// Writing half. Not `Clone`: there is exactly one producer.
pub struct Producer {
    shared: Arc<Shared>,
}

/// Reading half. Not `Clone`: there is exactly one consumer.
pub struct Consumer {
    shared: Arc<Shared>,
}

/// Create a ring holding up to `capacity` samples.
pub fn ring(capacity: usize) -> (Producer, Consumer) {
    assert!(capacity > 0, "ring capacity must be positive");
    let buf: Box<[UnsafeCell<f32>]> = (0..capacity).map(|_| UnsafeCell::new(0.0)).collect();
    let shared = Arc::new(Shared { buf, cap: capacity, head: AtomicUsize::new(0), tail: AtomicUsize::new(0), dropped: AtomicU64::new(0) });
    (Producer { shared: shared.clone() }, Consumer { shared })
}

impl Producer {
    /// Append as many samples as fit; returns how many were written. The
    /// rest are dropped and added to the overrun counter.
    pub fn push_slice(&mut self, data: &[f32]) -> usize {
        let s = &*self.shared;
        let head = s.head.load(Ordering::Relaxed);
        let tail = s.tail.load(Ordering::Acquire);
        let used = head.wrapping_sub(tail);
        let free = s.cap - used;
        let n = data.len().min(free);
        if n < data.len() {
            s.dropped.fetch_add((data.len() - n) as u64, Ordering::Relaxed);
        }
        if n > 0 {
            let start = head % s.cap;
            let first = n.min(s.cap - start);
            // SAFETY: slots [start, start+first) and [0, n-first) are free
            // (see the Sync impl) and within the buffer.
            unsafe {
                std::ptr::copy_nonoverlapping(data.as_ptr(), s.ptr().add(start), first);
                std::ptr::copy_nonoverlapping(data.as_ptr().add(first), s.ptr(), n - first);
            }
            s.head.store(head.wrapping_add(n), Ordering::Release);
        }
        n
    }

    pub fn capacity(&self) -> usize {
        self.shared.cap
    }
}

impl Consumer {
    /// Samples ready to read.
    pub fn available(&self) -> usize {
        let s = &*self.shared;
        s.head.load(Ordering::Acquire).wrapping_sub(s.tail.load(Ordering::Relaxed))
    }

    /// Read up to `out.len()` samples; returns how many were read.
    pub fn pop_slice(&mut self, out: &mut [f32]) -> usize {
        let s = &*self.shared;
        let tail = s.tail.load(Ordering::Relaxed);
        let head = s.head.load(Ordering::Acquire);
        let n = out.len().min(head.wrapping_sub(tail));
        if n > 0 {
            let start = tail % s.cap;
            let first = n.min(s.cap - start);
            // SAFETY: slots [tail, tail+n) were published by the producer's
            // Release store of head and are not written until tail advances.
            unsafe {
                std::ptr::copy_nonoverlapping(s.ptr().add(start), out.as_mut_ptr(), first);
                std::ptr::copy_nonoverlapping(s.ptr(), out.as_mut_ptr().add(first), n - first);
            }
            s.tail.store(tail.wrapping_add(n), Ordering::Release);
        }
        n
    }

    /// Discard everything currently queued.
    pub fn clear(&mut self) {
        let s = &*self.shared;
        let head = s.head.load(Ordering::Acquire);
        s.tail.store(head, Ordering::Release);
    }

    /// Samples dropped because the buffer was full (since creation).
    pub fn dropped(&self) -> u64 {
        self.shared.dropped.load(Ordering::Relaxed)
    }

    pub fn capacity(&self) -> usize {
        self.shared.cap
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn push_then_pop_preserves_order() {
        let (mut p, mut c) = ring(8);
        assert_eq!(p.push_slice(&[1.0, 2.0, 3.0]), 3);
        assert_eq!(c.available(), 3);
        let mut out = [0.0; 8];
        assert_eq!(c.pop_slice(&mut out), 3);
        assert_eq!(&out[..3], &[1.0, 2.0, 3.0]);
        assert_eq!(c.available(), 0);
        assert_eq!(c.pop_slice(&mut out), 0);
    }

    #[test]
    fn wraps_around_the_end() {
        let (mut p, mut c) = ring(5);
        let mut out = [0.0; 5];
        for round in 0..20 {
            let base = round as f32 * 10.0;
            assert_eq!(p.push_slice(&[base, base + 1.0, base + 2.0]), 3);
            assert_eq!(c.pop_slice(&mut out[..3]), 3);
            assert_eq!(&out[..3], &[base, base + 1.0, base + 2.0]);
        }
        assert_eq!(c.dropped(), 0);
    }

    #[test]
    fn full_buffer_drops_new_samples_and_counts_them() {
        let (mut p, mut c) = ring(4);
        assert_eq!(p.push_slice(&[1.0, 2.0, 3.0]), 3);
        assert_eq!(p.push_slice(&[4.0, 5.0, 6.0]), 1);
        assert_eq!(c.dropped(), 2);
        let mut out = [0.0; 4];
        assert_eq!(c.pop_slice(&mut out), 4);
        assert_eq!(out, [1.0, 2.0, 3.0, 4.0]);
        // Space is reclaimed after reading.
        assert_eq!(p.push_slice(&[7.0, 8.0]), 2);
    }

    #[test]
    fn clear_discards_queued_samples() {
        let (mut p, mut c) = ring(4);
        p.push_slice(&[1.0, 2.0]);
        c.clear();
        assert_eq!(c.available(), 0);
        p.push_slice(&[3.0]);
        let mut out = [0.0; 2];
        assert_eq!(c.pop_slice(&mut out), 1);
        assert_eq!(out[0], 3.0);
    }

    #[test]
    fn concurrent_producer_and_consumer_keep_every_sample_in_order() {
        const TOTAL: usize = 2_000_000;
        let (mut p, mut c) = ring(4096);
        let producer = std::thread::spawn(move || {
            let mut next = 0usize;
            let mut chunk = vec![0.0f32; 333];
            while next < TOTAL {
                let n = chunk.len().min(TOTAL - next);
                for (i, v) in chunk[..n].iter_mut().enumerate() {
                    *v = ((next + i) % 1_000_000) as f32;
                }
                let mut written = 0;
                while written < n {
                    written += p.push_slice(&chunk[written..n]);
                    if written < n {
                        std::thread::yield_now();
                    }
                }
                next += n;
            }
        });
        let mut expected = 0usize;
        let mut out = vec![0.0f32; 500];
        while expected < TOTAL {
            let n = c.pop_slice(&mut out);
            for v in &out[..n] {
                assert_eq!(*v, (expected % 1_000_000) as f32, "sample {expected}");
                expected += 1;
            }
            if n == 0 {
                std::thread::yield_now();
            }
        }
        producer.join().unwrap();
        // The producer retried instead of dropping, so nothing was counted
        // except the partial pushes that were retried.
        assert_eq!(c.available(), 0);
    }
}
