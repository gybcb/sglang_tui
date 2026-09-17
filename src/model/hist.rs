/// A fixed-capacity ring buffer of `f64` samples, allocation-free on the push
/// path. Backs every sparkline/braille graph. At 1 Hz a capacity of 1024 holds
/// ~17 minutes of history — wider than any real terminal at 2 samples/cell,
/// so the buffer is never the windowing constraint.
#[derive(Clone, Debug)]
pub struct Ring {
    buf: Vec<f64>,
    /// Next write position (also, when full, the oldest sample).
    head: usize,
    len: usize,
    cap: usize,
}

impl Ring {
    pub fn new(cap: usize) -> Self {
        let cap = cap.max(1);
        Ring {
            buf: vec![0.0; cap],
            head: 0,
            len: 0,
            cap,
        }
    }

    /// Push exactly one sample. Call once per poll tick, never per draw tick
    /// (the scroll illusion depends on one cell-shift per collected sample).
    #[inline]
    pub fn push(&mut self, v: f64) {
        self.buf[self.head] = v;
        self.head = (self.head + 1) % self.cap;
        if self.len < self.cap {
            self.len += 1;
        }
    }

    /// Number of valid samples (up to `cap`).
    #[inline]
    pub fn len(&self) -> usize {
        self.len
    }

    #[inline]
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Oldest-to-newest view of the last `n` samples (clamped to `len`).
    /// Returns a borrowed slice only when the window is contiguous; otherwise
    /// the caller gets the same data via [`Ring::tail_owned`].
    pub fn tail(&self, n: usize, out: &mut Vec<f64>) {
        out.clear();
        let n = n.min(self.len);
        // The window is the *newest* n samples, ending at the last write
        // (head-1), not the oldest n. Walking forward from `head - len`
        // instead would return the oldest n whenever n < len, so a "peak
        // over the visible window" readout could exclude the current sample
        // entirely once history is longer than the window (x > y).
        let start = (self.head + self.cap - n) % self.cap;
        out.reserve(n);
        for i in 0..n {
            out.push(self.buf[(start + i) % self.cap]);
        }
    }

    /// Owned last-`n` samples, oldest-first. Convenience wrapper over [`Ring::tail`].
    pub fn tail_owned(&self, n: usize) -> Vec<f64> {
        let mut v = Vec::new();
        self.tail(n, &mut v);
        v
    }

    /// Most recent sample, if any.
    #[inline]
    pub fn last(&self) -> Option<f64> {
        if self.len == 0 {
            None
        } else {
            Some(self.buf[(self.head + self.cap - 1) % self.cap])
        }
    }

    /// Max over the whole valid window (for btop's "peak" readout).
    pub fn max(&self) -> Option<f64> {
        if self.len == 0 {
            return None;
        }
        let start = (self.head + self.cap - self.len) % self.cap;
        let mut m = f64::MIN;
        for i in 0..self.len {
            let v = self.buf[(start + i) % self.cap];
            if v > m {
                m = v;
            }
        }
        Some(m)
    }
}

impl Default for Ring {
    fn default() -> Self {
        Ring::new(HIST_CAP)
    }
}

/// History capacity: enough for the widest conceivable terminal at 2
/// samples/braille-cell, with headroom.
pub const HIST_CAP: usize = 1024;

#[cfg(test)]
mod tests {
    use super::*;

    // Regression for the ENGINE `cur/max` readout: the max window must be the
    // *newest* n samples, so the current value is always inside it (x <= y).
    // The old `tail` computed the window start from `head - len` instead of
    // `head - n`, so once history is longer than the window it returned the
    // oldest n samples and the current sample fell out of the max.
    #[test]
    fn tail_window_is_newest_n_not_oldest_n() {
        let mut r = Ring::new(8);
        // Push 0,1,2,3,4,5,6,7 (fills), then 8,9 — wraps so len(=8) > n(=3).
        for i in 0..10 {
            r.push(i as f64);
        }
        assert_eq!(r.len(), 8, "ring full");
        let newest3 = r.tail_owned(3);
        assert_eq!(
            newest3,
            vec![7.0, 8.0, 9.0],
            "window = newest 3, not oldest 3"
        );
    }

    // The visible-window peak used by the `cur/max` readout must never be
    // below the current sample.
    #[test]
    fn window_peak_includes_the_current_sample() {
        let mut r = Ring::new(1024);
        // A long quiet stretch, a modest peak, then a value well above it.
        for _ in 0..300 {
            r.push(100.0);
        }
        r.push(500.0); // the old peak, now far behind the window
        for _ in 0..150 {
            r.push(100.0);
        }
        r.push(1000.0); // current, higher than the stale peak

        // A ~220s braille window (~440 samples) excludes the 500.0 peak.
        let window = 440;
        let mx = r.tail_owned(window).into_iter().fold(0.0f64, f64::max);
        assert!(
            mx >= 1000.0,
            "window peak {mx} must include current 1000 (cur<=peak)"
        );
    }
}
