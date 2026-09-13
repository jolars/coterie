//! Scoped wall time for crash schedules, independent of fsync and scheduling.

use std::cell::Cell;

thread_local! {
    static NOW_MS: Cell<Option<i64>> = const { Cell::new(None) };
}

pub(super) fn now_ms() -> Option<i64> {
    NOW_MS.get()
}

pub(super) struct FrozenClock {
    previous: Option<i64>,
    // Restoration must happen on the thread whose clock was frozen.
    _thread: std::marker::PhantomData<*const ()>,
}

impl FrozenClock {
    pub(super) fn new(now_ms: i64) -> Self {
        Self {
            previous: NOW_MS.replace(Some(now_ms)),
            _thread: std::marker::PhantomData,
        }
    }
}

impl Drop for FrozenClock {
    fn drop(&mut self) {
        NOW_MS.set(self.previous);
    }
}

#[test]
fn frozen_wall_time_is_scoped_and_thread_local() {
    assert_eq!(now_ms(), None);
    {
        let _clock = FrozenClock::new(12_345);
        assert_eq!(super::unix_timestamp_ms().unwrap(), 12_345);
        assert_eq!(super::unix_timestamp().unwrap(), 12);
        std::thread::spawn(|| assert_eq!(now_ms(), None))
            .join()
            .unwrap();
        {
            let _nested = FrozenClock::new(20_000);
            assert_eq!(super::unix_timestamp().unwrap(), 20);
        }
        assert_eq!(super::unix_timestamp_ms().unwrap(), 12_345);
    }
    assert_eq!(now_ms(), None);
}
