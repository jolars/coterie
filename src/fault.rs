//! Crash boundaries, inert outside explicitly armed test subprocesses.

#[inline]
pub(crate) fn point(_name: &'static str) {
    #[cfg(test)]
    injection::point(_name);
}

#[cfg(test)]
pub(crate) mod injection {
    use std::cell::RefCell;
    use std::fs::{File, OpenOptions};
    use std::io::Write;
    use std::path::Path;

    thread_local! {
        static PLAN: RefCell<Option<Plan>> = const { RefCell::new(None) };
        static ACTION: RefCell<Option<Action>> = const { RefCell::new(None) };
    }

    struct Action {
        name: &'static str,
        callback: Box<dyn FnOnce()>,
    }

    pub(crate) struct ActionGuard;

    impl Drop for ActionGuard {
        fn drop(&mut self) {
            ACTION.with_borrow_mut(|action| *action = None);
        }
    }

    /// Changes a test-owned resource at an exact side-effect boundary.
    pub(crate) fn on_point(
        name: &'static str,
        callback: impl FnOnce() + 'static,
    ) -> ActionGuard {
        ACTION.with_borrow_mut(|action| {
            assert!(action.is_none());
            *action = Some(Action {
                name,
                callback: Box::new(callback),
            });
        });
        ActionGuard
    }

    struct Plan {
        trace: File,
        remaining: Option<usize>,
        ignore: Option<&'static str>,
        delay: Option<(&'static str, std::time::Duration)>,
    }

    pub(crate) const CRASH_EXIT: i32 = 86;

    // Arming is explicit so ordinary parallel tests and provider reader threads
    // cannot inherit another test's injection configuration.
    pub(crate) fn arm(path: &Path, boundary: Option<usize>) {
        let trace = OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .unwrap();
        PLAN.with_borrow_mut(|plan| {
            assert!(plan.is_none());
            *plan = Some(Plan {
                trace,
                remaining: boundary,
                ignore: None,
                delay: None,
            });
        });
    }

    pub(crate) fn disarm() {
        PLAN.with_borrow_mut(|plan| *plan = None);
    }

    pub(crate) fn ignore_empty_poll_boundary() {
        PLAN.with_borrow_mut(|plan| {
            plan.as_mut().unwrap().ignore = Some("session.event.before");
        });
    }

    pub(crate) fn delay_once(
        name: &'static str,
        duration: std::time::Duration,
    ) {
        PLAN.with_borrow_mut(|plan| {
            plan.as_mut().unwrap().delay = Some((name, duration));
        });
    }

    pub(super) fn point(name: &'static str) {
        let action = ACTION.with_borrow_mut(|action| {
            if action.as_ref().is_some_and(|action| action.name == name) {
                action.take()
            } else {
                None
            }
        });
        if let Some(action) = action {
            (action.callback)();
        }
        PLAN.with_borrow_mut(|plan| {
            let Some(plan) = plan else { return };
            if plan.ignore == Some(name) {
                return;
            }
            writeln!(plan.trace, "{name}").unwrap();
            plan.trace.sync_data().unwrap();
            if let Some((delayed, duration)) = plan.delay
                && delayed == name
            {
                plan.delay = None;
                std::thread::sleep(duration);
            }
            if let Some(remaining) = &mut plan.remaining {
                if *remaining == 0 {
                    // Exit without unwinding: SQLite, Git, process handles, and
                    // temporary files must survive exactly as at this boundary.
                    std::process::exit(CRASH_EXIT);
                }
                *remaining -= 1;
            }
        });
    }
}
