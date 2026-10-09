use std::time::Duration;

pub struct Backoff {
    initial: Duration,
    max: Duration,
    current: Duration,
}

impl Backoff {
    pub fn new(initial: Duration, max: Duration) -> Self {
        Backoff { initial, max, current: initial }
    }

    pub fn next_delay(&mut self) -> Duration {
        let ceiling = self.current;
        self.current = (self.current * 2).min(self.max);
        let low = ceiling / 2;
        low + (ceiling - low).mul_f64(rand::random::<f64>())
    }

    pub fn reset(&mut self) {
        self.current = self.initial;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn doubles_with_jitter_up_to_the_cap_and_resets() {
        let mut b = Backoff::new(Duration::from_millis(500), Duration::from_secs(30));
        let first = b.next_delay();
        assert!(first >= Duration::from_millis(250) && first <= Duration::from_millis(500));
        for _ in 0..20 {
            assert!(b.next_delay() <= Duration::from_secs(30));
        }
        let late = b.next_delay();
        assert!(late >= Duration::from_secs(15));
        b.reset();
        assert!(b.next_delay() <= Duration::from_millis(500));
    }
}
