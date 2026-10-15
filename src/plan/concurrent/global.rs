use crate::plan::concurrent::Pause;
use crate::plan::Plan;
use crate::vm::{Collection, VMBinding};

/// Trait for a concurrent plan.
pub trait ConcurrentPlan: Plan {
    /// Return `true`` if concurrent work (such as concurrent marking) is in progress.
    fn concurrent_work_in_progress(&self) -> bool;
    /// Return the current pause kind.  `None` if not in a pause.
    fn current_pause(&self) -> Option<Pause>;
    /// Called when concurrent work is interrupted.
    fn on_concurrent_work_interrupted(&self);
    /// Called when the concurrent phase's work packets have drained: every GC worker has parked,
    /// so nothing is in progress, and no pause has been requested.
    fn on_concurrent_work_drained(&self);

    /// Leave `GcStatus::InConcurrentGC` because the concurrent phase ended without a pause, and
    /// tell the VM.  Plans call this from [`Self::on_concurrent_work_drained`] once no concurrent
    /// work is left.  Does nothing if a pause has been requested in the meantime.
    fn finish_concurrent_work_without_pause(&self) {
        if self
            .base()
            .global_state
            .gc_status
            .set_concurrent_gc_finished()
        {
            <Self::VM as VMBinding>::VMCollection::concurrent_work_finished_no_pause();
        }
    }
}
