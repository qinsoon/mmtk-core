use crate::plan::Mutator;
use crate::plan::Plan;
use crate::util::opaque_pointer::*;
use crate::vm::VMBinding;
use crate::util::metadata::side_metadata::SideMetadataSanity;
use std::marker::PhantomData;
use std::sync::MutexGuard;

pub struct SynchronizedMutatorIterator<'a, VM: VMBinding> {
    _guard: MutexGuard<'a, ()>,
    start: bool,
    phantom: PhantomData<VM>,
}

impl<'a, VM: VMBinding> Iterator for SynchronizedMutatorIterator<'a, VM> {
    type Item = &'static mut Mutator<VM>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.start {
            self.start = false;
            VM::VMActivePlan::reset_mutator_iterator();
        }
        VM::VMActivePlan::get_next_mutator()
    }
}

/// VM-specific methods for the current plan.
pub trait ActivePlan<VM: VMBinding> {
    /// Return a reference to the current plan.
    // TODO: I don't know how this can be implemented when we have multiple MMTk instances.
    // This function is used by space and phase to refer to the current plan.
    // Possibly we should remove the use of this function, and remove this function?
    fn global() -> &'static dyn Plan<VM = VM>;

    /// Return whether there is a mutator created and associated with the thread.
    ///
    /// Arguments:
    /// * `tls`: The thread to query.
    ///
    /// # Safety
    /// The caller needs to make sure that the thread is valid (a value passed in by the VM binding through API).
    fn is_mutator(tls: VMThread) -> bool;

    /// Return a `Mutator` reference for the thread.
    ///
    /// Arguments:
    /// * `tls`: The thread to query.
    ///
    /// # Safety
    /// The caller needs to make sure that the thread is a mutator thread.
    fn mutator(tls: VMMutatorThread) -> &'static mut Mutator<VM>;

    /// Reset the mutator iterator so that `get_next_mutator()` returns the first mutator.
    fn reset_mutator_iterator();

    /// Return the next mutator if there is any. This method assumes that the VM implements stateful type
    /// to remember which mutator is returned and guarantees to return the next when called again. This does
    /// not need to be thread safe.
    fn get_next_mutator() -> Option<&'static mut Mutator<VM>>;

    /// A utility method to provide a thread-safe mutator iterator from `reset_mutator_iterator()` and `get_next_mutator()`.
    fn mutators<'a>() -> SynchronizedMutatorIterator<'a, VM> {
        SynchronizedMutatorIterator {
            _guard: Self::global().base().mutator_iterator_lock.lock().unwrap(),
            start: true,
            phantom: PhantomData,
        }
    }

    /// Return the total count of mutators.
    fn number_of_mutators() -> usize;

    /// This method is intended for a binding to verify any side metadata they define (note this does not include any spec
    /// in `ObjectModel` that are side metadata).
    ///
    /// MMTk has a side metadata module, and MMTk internally uses it. However, the module
    /// is public, and a binding could use it to store metadata for their convinience. If a binding defines side metadata specs with MMTk,
    /// they are required to wrap the specs into [`SideMetadataContext`](crate::util::metadata::side_metadata::SideMetadataContext),
    /// and call `checker.verify_metadata_context("[your binding name or something]", your_context)` in this
    /// method. Otherwise, when the `extreme_assertions` feature is enabled, MMTk will panic as
    /// it is not aware of the side metadata defined by a binding.
    ///
    /// If a binding does not use MMTk's side metadata, they can leave this as empty (the default implementation).
    fn vm_verify_side_metadata_sanity(checker: &mut SideMetadataSanity) {

    }
}
