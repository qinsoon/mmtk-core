mod timer;
#[cfg(feature = "perf_counter")]
mod perf_counter;

/// Common struct for different work counters
///
/// Stores the total, min and max of counter readings
#[derive(Copy, Clone, Debug)]
pub(super) struct CounterBase {
    pub(super) total: f64,
    pub(super) min: f64,
    pub(super) max: f64,
}

/// Make [`Counter`] trait objects cloneable
pub(super) trait CounterClone {
    /// Clone the object
    fn clone_box(&self) -> Box<dyn Counter>;
}

impl<T: 'static + Counter + Clone> CounterClone for T {
    fn clone_box(&self) -> Box<dyn Counter> {
        Box::new(self.clone())
    }
}

/// An abstraction of work counters
///
/// Use for trait objects, as we have might have types of work counters for
/// the same work packet and the types are not statically known.
/// The overhead should be negligible compared with the cost of executing
/// a work packet.
pub(super) trait Counter: CounterClone + std::fmt::Debug + Send {
    // TODO: consolidate with crate::util::statistics::counter::Counter;
    /// Start the counter
    fn start(&mut self);
    /// Stop the counter
    fn stop(&mut self);
    /// Name of counter
    fn name(&self) -> &str;
    /// Return a reference to [`CounterBase`]
    fn get_base(&self) -> &CounterBase;
    /// Return a mutatable reference to [`CounterBase`]
    fn get_base_mut(&mut self) -> &mut CounterBase;
}

impl Clone for Box<dyn Counter> {
    fn clone(&self) -> Box<dyn Counter> {
        self.clone_box()
    }
}

impl Default for CounterBase {
    fn default() -> Self {
        CounterBase {
            total: 0.0,
            min: f64::INFINITY,
            max: f64::NEG_INFINITY,
        }
    }
}

impl CounterBase {
    /// Merge two [`CounterBase`], keep the semantics of the fields,
    /// and return a new object
    pub(super) fn merge(&self, other: &Self) -> Self {
        let min = self.min.min(other.min);
        let max = self.max.max(other.max);
        let total = self.total + other.total;
        CounterBase { total, min, max }
    }

    /// Merge two [`CounterBase`], modify the current object in place,
    /// and keep the semantics of the fields
    pub(super) fn merge_inplace(&mut self, other: &Self) {
        self.min = self.min.min(other.min);
        self.max = self.max.max(other.max);
        self.total += other.total;
    }

    /// Update the object based on a single value
    pub(super) fn merge_val(&mut self, val: f64) {
        self.min = self.min.min(val);
        self.max = self.max.max(val);
        self.total += val;
    }
}
