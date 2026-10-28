//! Local time.
//!
//! `now` is the one signal the event loop sets, once a second on the second.
//! Hours, minutes and seconds are memos over it, so a reader of the minutes
//! alone wakes once a minute: the second ticks, the memo recomputes to the
//! same minute, and the wave stops there.

use genoma_core::reactive::{Memo, Signal};
use jiff::{Timestamp, tz::TimeZone};

pub struct Clock {
    now: Signal<i64>,
    pub hour: Memo<i64>,
    pub minute: Memo<i64>,
    pub second: Memo<i64>,
}

fn local(seconds: i64) -> jiff::Zoned {
    Timestamp::from_second(seconds).unwrap_or(Timestamp::UNIX_EPOCH).to_zoned(TimeZone::system())
}

impl Clock {
    pub fn new() -> Clock {
        let now = Signal::new(Timestamp::now().as_second());
        let hour = Memo::new(move || local(now.get()).hour() as i64);
        let minute = Memo::new(move || local(now.get()).minute() as i64);
        let second = Memo::new(move || local(now.get()).second() as i64);
        Clock { now, hour, minute, second }
    }

    /// Called by the event loop on each second.
    pub fn tick(&self) {
        self.now.set(Timestamp::now().as_second());
    }

    /// How long until the next whole second: the loop's next wake-up.
    pub fn until_next_second() -> std::time::Duration {
        let nanos = Timestamp::now().subsec_nanosecond().max(0) as u64;
        // One millisecond past the boundary, so the tick lands in the new second.
        std::time::Duration::from_nanos(1_000_000_000 - nanos + 1_000_000)
    }
}

impl Default for Clock {
    fn default() -> Self {
        Clock::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use genoma_core::reactive::Effect;
    use std::{cell::Cell, rc::Rc};

    #[test]
    fn a_minute_reader_ignores_the_seconds() {
        let clock = Clock::new();
        let runs = Rc::new(Cell::new(0));
        let r = runs.clone();
        let minute = clock.minute;
        Effect::new(move || {
            minute.get();
            r.set(r.get() + 1);
        });
        // Same minute, a later second (unless the test straddles a minute).
        let base = clock.now.get_untracked().unwrap();
        let start = base - base.rem_euclid(60);
        for s in 1..30 {
            clock.now.set(start + s);
        }
        assert!(runs.get() <= 2, "woke {} times for one minute", runs.get());
        clock.now.set(start + 61);
        assert!(runs.get() >= 2);
    }
}
