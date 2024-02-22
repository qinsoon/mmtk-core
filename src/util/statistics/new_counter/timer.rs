use super::*;
use std::time::Instant;

/// Measure the durations of work packets
///
/// Timing is based on [`Instant`]
#[derive(Clone, Debug)]
pub(super) struct Timer {
    base: CounterBase,
    name: String,
    start_value: Option<Instant>,
    running: bool,
}

impl Timer {
    pub(super) fn new(name: String) -> Self {
        Timer {
            base: Default::default(),
            name,
            start_value: None,
            running: false,
        }
    }
}

impl Counter for Timer {
    fn start(&mut self) {
        self.start_value = Some(Instant::now());
        self.running = true;
    }

    fn stop(&mut self) {
        let duration = self.start_value.unwrap().elapsed().as_nanos() as f64;
        self.base.merge_val(duration);
    }

    fn name(&self) -> &str {
        &self.name
    }

    fn get_base(&self) -> &CounterBase {
        &self.base
    }

    fn get_base_mut(&mut self) -> &mut CounterBase {
        &mut self.base
    }
}
