//! Per-speaker reordering buffer: holds a few packets before playing,
//! conceals short losses, and resets after silence.
//!
//! A packet that is merely late (nothing after it has arrived yet) is waited
//! for: we conceal one frame and keep the slot, so the buffer grows by 20 ms
//! instead of throwing the packet away when it lands. Packets come in
//! bursts (two frames per NightCoop message, delivered on game frames), so
//! without this every bit of jitter turned into a loss.

use std::collections::BTreeMap;

/// Frames buffered before a speaker starts playing (60 ms): one 40 ms
/// message plus room for its delivery to wobble.
const START_DEPTH: usize = 3;
/// Above this the buffer is trimmed, to keep latency bounded.
const MAX_DEPTH: usize = 8;
/// Lost frames concealed before a speaker is considered silent.
const MAX_MISSING: u32 = 5;

#[derive(Debug, PartialEq)]
pub enum Next<T> {
    /// Speaker is silent.
    Silent,
    Packet(T),
    /// Expected packet missing: conceal it.
    Lost,
}

pub struct Jitter<T> {
    queue: BTreeMap<u32, T>,
    next: Option<u32>,
    missing: u32,
}

impl<T> Default for Jitter<T> {
    fn default() -> Self {
        Self { queue: BTreeMap::new(), next: None, missing: 0 }
    }
}

impl<T> Jitter<T> {
    pub fn push(&mut self, seq: u32, item: T) {
        if self.next.is_some_and(|n| seq < n) {
            return;
        }
        self.queue.insert(seq, item);
        while self.queue.len() > MAX_DEPTH {
            self.queue.pop_first();
            self.next = self.queue.first_key_value().map(|(k, _)| *k);
        }
    }

    pub fn pop(&mut self) -> Next<T> {
        let next = match self.next {
            Some(n) => n,
            None if self.queue.len() >= START_DEPTH => *self.queue.first_key_value().unwrap().0,
            None => return Next::Silent,
        };
        self.next = Some(next.wrapping_add(1));
        match self.queue.remove(&next) {
            Some(item) => {
                self.missing = 0;
                Next::Packet(item)
            }
            None => {
                self.missing += 1;
                if self.queue.is_empty() {
                    // Late, not lost (yet): wait for it.
                    self.next = Some(next);
                }
                if self.missing > MAX_MISSING {
                    self.next = None;
                    self.missing = 0;
                    Next::Silent
                } else {
                    Next::Lost
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn waits_reorders_conceals_and_resets() {
        let mut j = Jitter::default();
        j.push(10, 'a');
        j.push(12, 'c');
        assert_eq!(j.pop(), Next::Silent, "waits for three packets");
        j.push(11, 'b');
        assert_eq!(j.pop(), Next::Packet('a'));
        assert_eq!(j.pop(), Next::Packet('b'));
        assert_eq!(j.pop(), Next::Packet('c'));
        j.push(9, 'z');
        for _ in 0..MAX_MISSING {
            assert_eq!(j.pop(), Next::Lost);
        }
        assert_eq!(j.pop(), Next::Silent);
    }

    #[test]
    fn late_packets_are_waited_for_lost_ones_skipped() {
        let mut j = Jitter::default();
        for s in 0..3 {
            j.push(s, s);
        }
        for s in 0..3 {
            assert_eq!(j.pop(), Next::Packet(s));
        }
        // 3 is late: conceal, then play it when it lands.
        assert_eq!(j.pop(), Next::Lost);
        j.push(3, 3);
        assert_eq!(j.pop(), Next::Packet(3));
        // 4 is lost: 5 is already here, so skip past it.
        j.push(5, 5);
        assert_eq!(j.pop(), Next::Lost);
        assert_eq!(j.pop(), Next::Packet(5));
    }

    #[test]
    fn trims_when_too_deep() {
        let mut j = Jitter::default();
        for s in 0..20 {
            j.push(s, s);
        }
        assert_eq!(j.pop(), Next::Packet(12));
    }
}
