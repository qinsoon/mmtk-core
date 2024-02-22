//! Measure the perf events of work packets
//!
//! This is built on top of libpfm4.
//! The events to measure are parsed from MMTk option `perf_events`
use super::*;
use libc::{c_int, pid_t};
use pfm::PerfEvent;
use std::fmt;

/// Work counter for perf events
#[derive(Clone)]
pub struct PerfCounter {
    base: CounterBase,
    running: bool,
    event_name: String,
    pe: PerfEvent,
}

impl PerfCounter {
    /// Create a work counter
    ///
    /// See `perf_event_open` for more details on `pid` and `cpu`
    /// Examples:
    /// 0, -1 measures the calling thread on all CPUs
    /// -1, 0 measures all threads on CPU 0
    /// -1, -1 is invalid
    pub fn new(name: &str, pid: pid_t, cpu: c_int, exclude_kernel: bool) -> PerfCounter {
        let mut pe = PerfEvent::new(name, false)
            .unwrap_or_else(|_| panic!("Failed to create perf event {}", name));
        pe.set_exclude_kernel(exclude_kernel as u64);
        pe.open(pid, cpu)
            .unwrap_or_else(|_| panic!("Failed to open perf event {}", name));
        PerfCounter {
            base: Default::default(),
            running: false,
            event_name: name.to_string(),
            pe,
        }
    }
}

impl fmt::Debug for PerfCounter {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("WorkPerfEvent")
            .field("base", &self.base)
            .field("running", &self.running)
            .field("event_name", &self.event_name)
            .finish()
    }
}

impl Counter for PerfCounter {
    fn start(&mut self) {
        self.running = true;
        self.pe.reset().expect("Failed to reset perf event");
        self.pe.enable().expect("Failed to enable perf event");
    }
    fn stop(&mut self) {
        self.running = true;
        let perf_event_value = self.pe.read().unwrap();
        self.base.merge_val(perf_event_value.value as f64);
        // assert not multiplexing
        assert_eq!(perf_event_value.time_enabled, perf_event_value.time_running);
        self.pe.disable().expect("Failed to disable perf event");
    }
    fn name(&self) -> &str {
        &self.event_name
    }
    fn get_base(&self) -> &CounterBase {
        &self.base
    }
    fn get_base_mut(&mut self) -> &mut CounterBase {
        &mut self.base
    }
}