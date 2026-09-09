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
    }

    struct Plan {
        trace: File,
        remaining: Option<usize>,
        ignore: Option<&'static str>,
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

    pub(super) fn point(name: &'static str) {
        PLAN.with_borrow_mut(|plan| {
            let Some(plan) = plan else { return };
            if plan.ignore == Some(name) {
                return;
            }
            writeln!(plan.trace, "{name}").unwrap();
            plan.trace.sync_data().unwrap();
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
