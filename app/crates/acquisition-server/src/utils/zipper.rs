//! Best-effort timestamp-based zipper across N monotonic streams. The
//! producer pushes `(item, stream_id, timestamp_ns)` and the zipper emits
//! grouped tuples — one item per stream — chosen to minimize per-group
//! timestamp spread.
//!
//! # Per-stream contract
//!
//! Pushes within a stream MUST be strictly increasing in timestamp. A push
//! with `ts ≤ last_ts` for that stream is rejected as an `OutOfOrder` error;
//! the offending item never enters the buffer and the stream's `last_ts` is
//! not advanced.
//!
//! # Grouping rule (the safe group)
//!
//! A group `[s0_i, s1_j, ..., sN_k]` (one item per stream) is *safe to emit*
//! when **both** hold for every ordered pair of streams `(s, t)` with `s ≠ t`:
//!
//! - **Mutual-best**: the chosen item from stream `s` is the closest item in
//!   stream `s`'s buffer to the chosen item from stream `t`
//! - **Locked-in**: stream `s`'s `last_ts ≥ ts` of the chosen item from
//!   stream `t` — future items in `s`, being strictly greater than `last_ts`,
//!   cannot be closer to `t`'s chosen item than the buffer's current best
//!
//! A group satisfying both conditions for all pairs is locally optimal and
//! waiting longer cannot improve it.
//!
//! # Stranding
//!
//! When a safe group is emitted with any stream's chosen index `> 0`, the
//! front items skipped over in that stream are ejected as `Stranded` errors
//! *before* the `Ok` group. Under the monotonicity contract those skipped
//! items can no longer be any future arrival's closest match, so they are
//! surfaced immediately rather than left to drift toward buffer overflow.
//!
//! # Buffer pressure (the patience window)
//!
//! `max_buffer_len` is the patience knob: how many items we'll hold while
//! waiting for a better match before giving up. When a buffer exceeds it and
//! no safe group exists:
//!
//! - if any other stream is empty → evict the overflowing stream's oldest
//!   item (its front) as `BufferOverflow` (incomplete group, no partner
//!   available)
//! - otherwise → emit a *forced group* anchored at the overflowing stream's
//!   oldest item (its front), pairing it with the closest item from each
//!   other stream (no mutual-best, no locked-in check), stranding any items
//!   older than the chosen items in each stream. This keeps groups flowing
//!   when streams are persistently out of sync, with per-item timestamp
//!   deltas in `Zipped.items` left for the consumer to inspect.
//!
//! # Output order
//!
//! `push` returns, in order: any `OutOfOrder` error for this push (if
//! rejected), then a sequence of (Stranded errors immediately preceding their
//! triggering `Ok` group) interleaved with `BufferOverflow` evictions, until
//! no further progress is possible.

use std::collections::VecDeque;

pub struct Zipped<T> {
    /// items[i] = (item, timestamp_ns) from stream i.
    pub items: Vec<(T, u64)>,
}

pub struct ZipError<T> {
    pub stream_id: usize,
    pub item: (T, u64),
    pub reason: ZipErrorReason,
}

pub enum ZipErrorReason {
    /// A stream buffer exceeded its maximum length; the oldest item was evicted.
    BufferOverflow,
    /// A push violated the per-stream chronological-order contract
    /// (timestamp ≤ last seen timestamp for that stream). The offending item
    /// was rejected; the stream's last_ts is unchanged.
    OutOfOrder,
    /// The item was the front of its stream's buffer but was passed over when
    /// a later item from the same stream was emitted in a mutual-best pair.
    /// Under the chronological-order contract no future arrival can make this
    /// item the closest match for any partner, so it is ejected immediately.
    Stranded,
}

pub struct StreamZipper<T> {
    n_streams: usize,
    buffers: Vec<VecDeque<(T, u64)>>,
    max_buffer_len: usize,
    last_ts: Vec<Option<u64>>,
}

impl<T> StreamZipper<T> {
    pub fn new(n_streams: usize, max_buffer_len: usize) -> Self {
        StreamZipper {
            n_streams,
            buffers: (0..n_streams).map(|_| VecDeque::new()).collect(),
            max_buffer_len,
            last_ts: vec![None; n_streams],
        }
    }

    /// Push an item from `stream_id` and return any results that are now ready.
    /// If the timestamp is not strictly greater than the stream's previous
    /// timestamp the item is rejected and a single `OutOfOrder` error is
    /// returned; otherwise the buffer is drained and all newly unlocked groups
    /// (and any stranded/overflow errors) are returned.
    pub fn push(&mut self, item: T, stream_id: usize, timestamp_ns: u64) -> Vec<Result<Zipped<T>, ZipError<T>>> {
        if self.last_ts[stream_id].is_some_and(|last| timestamp_ns <= last) {
            return vec![Err(ZipError {
                stream_id,
                item: (item, timestamp_ns),
                reason: ZipErrorReason::OutOfOrder,
            })];
        }
        self.last_ts[stream_id] = Some(timestamp_ns);
        self.buffers[stream_id].push_back((item, timestamp_ns));
        self.drain()
    }

    fn drain(&mut self) -> Vec<Result<Zipped<T>, ZipError<T>>> {
        let mut results = vec![];
        loop {
            // Try to find n-way mutual best-hit and safe to emit group
            if let Some(indices) = self.find_safe_group() {
                results.extend(self.strand_before(&indices));
                let items = self.pop_group();
                results.push(Ok(Zipped { items }));
                continue;
            }

            // Prevent buffer overflow
            if let Some(stream_id) = self
                .buffers
                .iter()
                .position(|b| b.len() > self.max_buffer_len)
            {
                let some_other_empty = (0..self.n_streams)
                    .filter(|&s| s != stream_id)
                    .any(|s| self.buffers[s].is_empty());
                if some_other_empty {
                    results.push(Err(ZipError {
                        stream_id,
                        item: self.buffers[stream_id].pop_front().unwrap(),
                        reason: ZipErrorReason::BufferOverflow,
                    }));
                    continue;
                }
                // Buffer pressure exceeded the patience window — emit a forced
                // group anchored at the overflowing stream's oldest item,
                // paired with the closest item from each other stream.
                let s_front_ts = self.buffers[stream_id][0].1;
                let mut indices = vec![0usize; self.n_streams];
                for s in 0..self.n_streams {
                    if s != stream_id {
                        indices[s] = Self::closest_in_stream(&self.buffers[s], s_front_ts);
                    }
                }
                results.extend(self.strand_before(&indices));
                let items = self.pop_group();
                results.push(Ok(Zipped { items }));
                continue;
            }

            break;
        }

        results
    }

    /// For each stream `s`, pop and strand the `indices[s]` oldest items.
    fn strand_before(&mut self, indices: &[usize]) -> Vec<Result<Zipped<T>, ZipError<T>>> {
        let mut out = vec![];
        for s in 0..self.n_streams {
            for _ in 0..indices[s] {
                out.push(Err(ZipError {
                    stream_id: s,
                    item: self.buffers[s].pop_front().unwrap(),
                    reason: ZipErrorReason::Stranded,
                }));
            }
        }
        out
    }

    /// Pop the front item from every stream and return them as a group.
    fn pop_group(&mut self) -> Vec<(T, u64)> {
        (0..self.n_streams)
            .map(|s| self.buffers[s].pop_front().unwrap())
            .collect()
    }

    /// Locate a safe-to-emit group (one index per stream) under the
    /// mutual-best + locked-in conditions for all pairs. Iterates stream 0 in
    /// index order so the earliest-anchored group is returned first, giving
    /// roughly chronological emission.
    fn find_safe_group(&self) -> Option<Vec<usize>> {
        if self.buffers.iter().any(|b| b.is_empty()) {
            return None;
        }
        let last_ts: Vec<u64> = self.last_ts.iter().copied().collect::<Option<Vec<_>>>()?;

        for (anchor_idx, &(_, anchor_ts)) in self.buffers[0].iter().enumerate() {
            let mut indices = vec![0usize; self.n_streams];
            indices[0] = anchor_idx;
            for s in 1..self.n_streams {
                indices[s] = Self::closest_in_stream(&self.buffers[s], anchor_ts);
            }
            if self.is_safe_group(&indices, &last_ts) {
                return Some(indices);
            }
        }
        None
    }

    /// Returns true when the candidate group satisfies the mutual-best and
    /// locked-in conditions for every ordered pair of streams.
    fn is_safe_group(&self, indices: &[usize], last_ts: &[u64]) -> bool {
        for s in 0..self.n_streams {
            for t in 0..self.n_streams {
                if s == t {
                    continue;
                }
                let t_ts = self.buffers[t][indices[t]].1;
                // Locked-in: future items in s are all > last_ts[s], so they
                // cannot be closer to t's chosen item than the current best.
                if last_ts[s] < t_ts {
                    return false;
                }
                // Mutual-best: the chosen item in s is the closest to t's item.
                if Self::closest_in_stream(&self.buffers[s], t_ts) != indices[s] {
                    return false;
                }
            }
        }
        true
    }

    fn closest_in_stream(buf: &VecDeque<(T, u64)>, ts: u64) -> usize {
        buf.iter()
            .enumerate()
            .min_by_key(|(_, (_, t))| t.abs_diff(ts))
            .map(|(k, _)| k)
            .unwrap()
    }
}

// TODO: review the unit tests
// TODO: extra unit tests
// - N = 3
// - Slow stream that catches up (N = 2 and N = 3)
// - Others?
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn perfect_sync_two_streams() {
        let mut z: StreamZipper<u64> = StreamZipper::new(2, 10);
        for i in 0u64..3 {
            let ts = 1_000_000_000 * (i + 1);
            let no_group = z.push(ts, 0, ts);
            assert!(no_group.is_empty());
            let groups = z.push(ts, 1, ts);
            assert_eq!(groups.len(), 1);
            let g = groups[0].as_ref().ok().expect("expected Ok group");
            assert_eq!(g.items[0].1, ts);
            assert_eq!(g.items[1].1, ts);
        }
    }

    #[test]
    fn one_stream_runs_ahead_at_startup() {
        let mut z: StreamZipper<u64> = StreamZipper::new(2, 10);
        for i in 0u64..3 {
            let ts = 1_000_000_000 * (i + 1);
            let groups = z.push(ts, 0, ts);
            assert!(groups.is_empty(), "no group possible without stream 1");
        }
        let mut groups = vec![];
        for i in 0u64..3 {
            let ts = 1_000_000_000 * (i + 1);
            groups.extend(z.push(ts, 1, ts));
        }
        let ok_count = groups.iter().filter(|r| r.is_ok()).count();
        assert_eq!(ok_count, 3);
    }

    #[test]
    fn stream_dropout_triggers_buffer_overflow_errors() {
        let mut z: StreamZipper<u64> = StreamZipper::new(2, 3);
        let mut results = vec![];
        for i in 0u64..=3 {
            let ts = 1_000_000_000 * (i + 1);
            results.extend(z.push(ts, 0, ts));
        }
        let overflow_count = results
            .iter()
            .filter(|r| {
                matches!(
                    r,
                    Err(ZipError {
                        reason: ZipErrorReason::BufferOverflow,
                        ..
                    })
                )
            })
            .count();
        assert!(overflow_count >= 1, "overflow should produce at least one error");
    }

    #[test]
    fn wildly_out_of_sync_still_emits_pairs() {
        // Streams are ~10 s apart and never overlap in time, so the locked-in
        // condition can't be met by mutual-best. With buffer pressure (15
        // pushes, max=3) the force-pair path takes over and emits pairs in
        // chronological order.
        let mut z: StreamZipper<u64> = StreamZipper::new(2, 3);
        let mut results = vec![];
        for i in 0u64..15 {
            let ts0 = 100_000_000 * (i + 1);
            let ts1 = 10_000_000_000 + 100_000_000 * (i + 1);
            results.extend(z.push(ts0, 0, ts0));
            results.extend(z.push(ts1, 1, ts1));
        }
        let ok_count = results.iter().filter(|r| r.is_ok()).count();
        assert!(
            ok_count > 0,
            "buffer pressure should force pair emission under extreme skew"
        );
        for g in results.iter().flatten() {
            let delta = g.items[0].1.abs_diff(g.items[1].1);
            assert!(
                (9_000_000_000..=11_000_000_000).contains(&delta),
                "delta {delta} should reflect the ~10s skew"
            );
        }
    }

    #[test]
    fn deferred_emission_waits_for_better_match() {
        let mut z: StreamZipper<u64> = StreamZipper::new(2, 10);
        let mut early = z.push(10, 0, 2_000_000_000);
        early.extend(z.push(20, 1, 1_000_000_000));
        assert!(
            early.is_empty(),
            "no pair should be emitted while the better B match might still arrive"
        );

        let results = z.push(21, 1, 2_000_000_000);
        assert_eq!(results.len(), 2);

        let stranded = results[0]
            .as_ref()
            .err()
            .expect("first result should be Stranded(B@1s)");
        assert!(matches!(stranded.reason, ZipErrorReason::Stranded));
        assert_eq!(stranded.stream_id, 1);
        assert_eq!(stranded.item.1, 1_000_000_000);

        let group = results[1].as_ref().ok().expect("second result should be Ok");
        assert_eq!(group.items[0].1, 2_000_000_000);
        assert_eq!(group.items[1].1, 2_000_000_000);
    }

    #[test]
    fn out_of_order_push_emits_error_and_preserves_last_ts() {
        let mut z: StreamZipper<u64> = StreamZipper::new(2, 10);
        let no_result = z.push(10, 0, 1000);
        assert!(no_result.is_empty());
        let after_ooo = z.push(99, 0, 500);
        assert_eq!(after_ooo.len(), 1);
        let err = after_ooo[0].as_ref().err().expect("OOO push should emit error");
        assert!(matches!(err.reason, ZipErrorReason::OutOfOrder));
        assert_eq!(err.stream_id, 0);
        assert_eq!(err.item.0, 99);

        let results = z.push(20, 1, 1000);
        assert_eq!(results.len(), 1);
        let group = results[0]
            .as_ref()
            .ok()
            .expect("the original A@1000 should still be in the buffer and pair now");
        assert_eq!(group.items[0].1, 1000);
        assert_eq!(group.items[1].1, 1000);
    }

    #[test]
    fn equal_timestamp_push_is_out_of_order() {
        let mut z: StreamZipper<u64> = StreamZipper::new(2, 10);
        let mut results = z.push(1, 0, 100);
        results.extend(z.push(2, 0, 100));
        let ooo_count = results
            .iter()
            .filter(|r| {
                matches!(
                    r,
                    Err(ZipError {
                        reason: ZipErrorReason::OutOfOrder,
                        ..
                    })
                )
            })
            .count();
        assert_eq!(ooo_count, 1, "equal-ts push should be one OOO error");
    }
}
