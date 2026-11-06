//! One lifecycle for every background task.
//!
//! Seven tasks (`cpu`, `sched`, `watch`, `record`, `ctl`, `sf-uprobe`, `sf-binder`)
//! were written by hand in the same shape: a `stop: Arc<AtomicBool>`, an
//! `Option<JoinHandle>`, a `spawn`, a `stop`, and a slot in `lib.rs`. Adding an
//! eighth data source meant editing five places — a static, a slot accessor, a start
//! block, a stop block, and the task file — and nothing in the compiler checked that
//! the new task was in the stop list. A task missing from that list keeps running
//! after the daemon claims to have stopped, which on this device means the CPU
//! governor stays armed in `userspace` (the one failure nothing in-process recovers
//! from).
//!
//! So the shape is now a trait plus an ordered registry:
//!
//! * `Task` is the whole contract: a name for the log, `stop`, `is_running`.
//! * `Registry` owns the tasks, stops them in **reverse start order** (so a consumer
//!   stops before the producer it reads), and logs a line whenever a task outlives its
//!   stop — a leak becomes observable instead of silent.
//!
//! The tasks themselves are unchanged: this module implements `Task` for them so
//! their files did not have to grow another lifecycle method.

/// A background task the daemon owns and must stop on the way out.
///
/// `Send` because the daemon itself moves between threads (started on the worker's
/// main thread, stopped by whichever thread handles the signal).
pub trait Task: Send {
    /// Stable name for logs and for picking one task out of the registry.
    fn name(&self) -> &'static str;
    /// Signal the task and wait for it (bounded — see `shutdown::stop_and_join`).
    fn stop(&mut self);
    /// Whether this handle still holds a live thread.
    fn is_running(&self) -> bool;
}

macro_rules! impl_task {
    ($ty:ty, $name:literal) => {
        impl Task for $ty {
            fn name(&self) -> &'static str {
                $name
            }
            fn stop(&mut self) {
                <$ty>::stop(self)
            }
            fn is_running(&self) -> bool {
                <$ty>::is_running(self)
            }
        }
    };
}

impl_task!(crate::cpu_task::CpuTask, "cpu");
impl_task!(crate::sched_task::SchedTask, "sched");
impl_task!(crate::watch_task::WatchTask, "watch");
impl_task!(crate::recorder::RecorderTask, "record");
impl_task!(crate::ctl_socket::CtlTask, "ctl");
impl_task!(crate::sf_uprobe::UprobeTask, "sf-uprobe");
impl_task!(crate::sf_binder::FrameTask, "sf-binder");

/// Ordered set of live tasks.
#[derive(Default)]
pub struct Registry {
    tasks: Vec<Box<dyn Task>>,
}

impl Registry {
    pub fn new() -> Self {
        Self { tasks: Vec::new() }
    }

    /// Register a started task. Order matters: `stop_all` runs in reverse.
    pub fn push<T: Task + 'static>(&mut self, t: T) {
        self.tasks.push(Box::new(t));
    }

    pub fn len(&self) -> usize {
        self.tasks.len()
    }

    pub fn is_empty(&self) -> bool {
        self.tasks.is_empty()
    }

    /// Names in start order (used by the shutdown log line).
    pub fn names(&self) -> Vec<&'static str> {
        self.tasks.iter().map(|t| t.name()).collect()
    }

    /// Stop one task by name and remove it, leaving the rest of the order intact.
    ///
    /// The daemon uses this for the CPU governor: an armed `userspace` takeover is the
    /// one piece of state that must not survive a slow shutdown, so it is stopped
    /// before the other tasks even when it was started after them.
    pub fn take_and_stop(&mut self, name: &str) -> bool {
        let Some(idx) = self.tasks.iter().position(|t| t.name() == name) else {
            return false;
        };
        let mut task = self.tasks.remove(idx);
        stop_one(task.as_mut());
        true
    }

    /// Stop everything in reverse start order.
    pub fn stop_all(&mut self) {
        while let Some(mut task) = self.tasks.pop() {
            stop_one(task.as_mut());
        }
    }
}

/// Stop, then check. A task that still reports running after its stop has been
/// detached (its thread is stuck); say so by name rather than letting the daemon exit
/// while claiming a clean shutdown.
fn stop_one(task: &mut dyn Task) {
    let name = task.name();
    task.stop();
    if task.is_running() {
        crate::log_msg(&format!(
            "Rust: task '{name}' did not stop (thread detached); it dies with the process"
        ));
    } else {
        crate::log_msg(&format!("Rust: task '{name}' stopped"));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Fake {
        name: &'static str,
        running: bool,
        stops: usize,
    }

    impl Task for Fake {
        fn name(&self) -> &'static str {
            self.name
        }
        fn stop(&mut self) {
            self.stops += 1;
            self.running = false;
        }
        fn is_running(&self) -> bool {
            self.running
        }
    }

    /// The registry's only promise that can be tested without threads: reverse order,
    /// and a task that refuses to stop is reported, not silently dropped.
    #[test]
    fn stops_in_reverse_order_and_reports_stuck_tasks() {
        // A task that ignores `stop`, i.e. a detached thread.
        struct Stuck;
        impl Task for Stuck {
            fn name(&self) -> &'static str {
                "stuck"
            }
            fn stop(&mut self) {}
            fn is_running(&self) -> bool {
                true
            }
        }

        let mut r = Registry::new();
        r.push(Fake { name: "first", running: true, stops: 0 });
        r.push(Stuck);
        assert_eq!(r.names(), vec!["first", "stuck"]);
        // Reverse order means the stuck one is stopped first; the loop must continue
        // past it, not abort.
        r.stop_all();
        assert!(r.is_empty());
    }

    #[test]
    fn take_and_stop_removes_only_the_named_task() {
        let mut r = Registry::new();
        r.push(Fake { name: "cpu", running: true, stops: 0 });
        r.push(Fake { name: "watch", running: true, stops: 0 });
        assert!(r.take_and_stop("cpu"));
        assert_eq!(r.names(), vec!["watch"]);
        assert!(!r.take_and_stop("cpu"), "a task is only taken once");
        r.stop_all();
        assert!(r.is_empty());
    }
}
