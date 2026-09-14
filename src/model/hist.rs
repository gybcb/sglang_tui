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
        // oldest index = (head - len) mod cap; walk forward n samples ending at head.
        let start = (self.head + self.cap - self.len) % self.cap;
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
