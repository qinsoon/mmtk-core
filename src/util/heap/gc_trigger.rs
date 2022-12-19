use atomic::Ordering;

use crate::plan::Plan;
use crate::policy::space::Space;
use crate::util::constants::LOG_BYTES_IN_PAGE;
use crate::util::options::{GCTriggerSelector, Options};
use crate::vm::VMBinding;
use crate::MMTK;
use std::mem::MaybeUninit;
use std::sync::atomic::AtomicUsize;

/// GCTrigger is responsible for triggering GCs based on the given policy.
/// All the decisions about heap limit and GC triggering should be resolved here.
/// Depending on the actual policy, we may either forward the calls either to the plan
/// or to the binding/runtime.
pub struct GCTrigger<VM: VMBinding> {
    /// The current plan. This is uninitialized when we create it, and later initialized
    /// once we have a fixed address for the plan.
    plan: MaybeUninit<&'static dyn Plan<VM = VM>>,
    /// The triggering policy.
    pub policy: Box<dyn GCTriggerPolicy<VM>>,
}

impl<VM: VMBinding> GCTrigger<VM> {
    pub fn new(options: &Options) -> Self {
        GCTrigger {
            plan: MaybeUninit::uninit(),
            policy: match *options.gc_trigger {
                GCTriggerSelector::FixedHeapSize(size) => Box::new(FixedHeapSizeTrigger {
                    total_pages: size >> LOG_BYTES_IN_PAGE,
                }),
                GCTriggerSelector::DynamicHeapSize(min, max) => Box::new(MemBalancerTrigger::new(
                    min >> LOG_BYTES_IN_PAGE,
                    max >> LOG_BYTES_IN_PAGE,
                )),
                GCTriggerSelector::Delegated => unimplemented!(),
            },
        }
    }

    /// Set the plan. This is called in `create_plan()` after we created a boxed plan.
    pub fn set_plan(&mut self, plan: &'static dyn Plan<VM = VM>) {
        self.plan.write(plan);
    }

    /// This method is called periodically by the allocation subsystem
    /// (by default, each time a page is consumed), and provides the
    /// collector with an opportunity to collect.
    ///
    /// Arguments:
    /// * `space_full`: Space request failed, must recover pages within 'space'.
    /// * `space`: The space that triggered the poll. This could `None` if the poll is not triggered by a space.
    pub fn poll(&self, space_full: bool, space: Option<&dyn Space<VM>>) -> bool {
        let plan = unsafe { self.plan.assume_init() };
        if self.policy.is_gc_required(space_full, space, plan) {
            info!(
                "[POLL] {}{}",
                if let Some(space) = space {
                    format!("{}: ", space.get_name())
                } else {
                    "".to_string()
                },
                "Triggering collection"
            );
            plan.base().gc_requester.request();
            return true;
        }
        false
    }

    /// Check if the heap is full
    pub fn is_heap_full(&self) -> bool {
        let plan = unsafe { self.plan.assume_init() };
        self.policy.is_heap_full(plan)
    }
}

/// This trait describes a GC trigger policy. A triggering policy have hooks to be informed about
/// GC start/end so they can collect some statistics about GC and allocation. The policy needs to
/// decide the (current) heap limit and decide whether a GC should be performed.
pub trait GCTriggerPolicy<VM: VMBinding>: Sync + Send {
    /// Inform the triggering policy that a GC starts.
    fn on_gc_start(&self, _mmtk: &'static MMTK<VM>) {}
    /// Inform the triggering policy that a GC is about to start the release work. This is called
    /// in the global [`scheduler::gc_work::Release`] work packet. This means we assume a plan
    /// do not schedule any work that reclaims memory before the global `Release` work. The current plans
    /// satisfy this assumption: they schedule other release work in `plan.release()`.
    fn on_gc_release(&self, _mmtk: &'static MMTK<VM>) {}
    /// Inform the triggering policy that a GC ends.
    fn on_gc_end(&self, _mmtk: &'static MMTK<VM>) {}
    /// Is a GC required now?
    fn is_gc_required(
        &self,
        space_full: bool,
        space: Option<&dyn Space<VM>>,
        plan: &dyn Plan<VM = VM>,
    ) -> bool;
    /// Is current heap full?
    fn is_heap_full(&self, plan: &'static dyn Plan<VM = VM>) -> bool;
    /// Return the current heap size (in pages)
    fn get_heap_size_in_pages(&self) -> usize;
    /// Can the heap size grow?
    fn can_heap_size_grow(&self) -> bool;
}

/// A simple GC trigger that uses a fixed heap size.
pub struct FixedHeapSizeTrigger {
    total_pages: usize,
}
impl<VM: VMBinding> GCTriggerPolicy<VM> for FixedHeapSizeTrigger {
    fn is_gc_required(
        &self,
        space_full: bool,
        space: Option<&dyn Space<VM>>,
        plan: &dyn Plan<VM = VM>,
    ) -> bool {
        // Let the plan decide
        plan.collection_required(space_full, space)
    }

    fn is_heap_full(&self, plan: &'static dyn Plan<VM = VM>) -> bool {
        // If reserved pages is larger than the total pages, the heap is full.
        plan.get_reserved_pages() > self.total_pages
    }

    fn get_heap_size_in_pages(&self) -> usize {
        self.total_pages
    }

    fn can_heap_size_grow(&self) -> bool {
        false
    }
}

use atomic::Atomic;
use std::time::Instant;

/// An implementation of MemBalancer (Optimal heap limits for reducing browser memory use, https://dl.acm.org/doi/10.1145/3563323)
/// We use MemBalancer to decide a heap limit between the min heap and the max heap.
/// The current implementation is a simplified version of mem balancer and it does not take collection/allocation speed into account,
/// and uses a fixed constant instead.
// TODO: implement a complete mem balancer.
pub struct MemBalancerTrigger {
    /// The min heap size
    min_heap_pages: usize,
    /// The max heap size
    max_heap_pages: usize,
    /// The current heap size
    current_heap_pages: AtomicUsize,
    /// Statistics
    stats: Atomic<MemBalancerStats>,
}

#[derive(Copy, Clone, Debug)]
struct MemBalancerStats {
    // Allocation/collection stats in the previous estimation. We keep this so we can use them to smooth the current value

    /// Previous allocated memory in pages.
    allocation_pages_prev: f64,
    /// Previous allocation duration in secs
    allocation_time_prev: f64,
    /// Previous collected memory in pages
    collection_pages_prev: f64,
    /// Previous colleciton duration in secs
    collection_time_prev: f64,

    // Allocation/collection stats in this estimation.

    /// Allocated memory in pages
    allocation_pages: f64,
    /// Allocation duration in secs
    allocation_time: f64,
    /// Collected memory in pages
    collection_pages: f64,
    /// Collection duration in secs
    collection_time: f64,

    /// The time when this GC starts
    gc_start_time: Instant,
    /// The time when this GC ends
    gc_end_time: Instant,

    /// The live pages before we release memory.
    gc_release_live_pages: usize,
    /// The live pages at the GC end
    gc_end_live_pages: usize,
}

/// A sentinel value for previous allocation/collection stats to indicate that no previous stats are recorded
const NO_PREV: f64 = -1f64;

impl std::default::Default for MemBalancerStats {
    fn default() -> Self {
        let now = Instant::now();
        Self {
            allocation_pages_prev: NO_PREV,
            allocation_time_prev: NO_PREV,
            collection_pages_prev: NO_PREV,
            collection_time_prev: NO_PREV,
            allocation_pages: 0f64,
            allocation_time: 0f64,
            collection_pages: 0f64,
            collection_time: 0f64,
            gc_start_time: now,
            gc_end_time: now,
            gc_release_live_pages: 0,
            gc_end_live_pages: 0,
        }
    }
}

impl MemBalancerStats {
    // Collect mem stats for generational plans:
    // * We ignore nursery GCs.
    // * allocation = objects in mature space = promoted + pretentured = live pages in mature space before release - live pages at the end of last mature GC
    // * collection = live pages in mature space at the end of GC -  live pages in mature space before release

    fn generational_mem_stats_on_gc_start<VM: VMBinding>(&mut self, _mmtk: &'static MMTK<VM>) {
        // We don't need to do anything
    }
    fn generational_mem_stats_on_gc_release<VM: VMBinding>(&mut self, mmtk: &'static MMTK<VM>) {
        if !mmtk.plan.is_current_gc_nursery() {
            self.gc_release_live_pages = mmtk.plan.get_mature_reserved_pages();

            // Calculate the promoted pages (including pre tentured objects)
            let promoted = self.gc_release_live_pages - self.gc_end_live_pages;
            self.allocation_pages = promoted as f64;
            trace!("promoted = mature live before release {} - mature live at prev gc end {} = {}", self.gc_release_live_pages, self.gc_end_live_pages, promoted);
            trace!("allocated pages (accumulated to) = {}", self.allocation_pages);
        }
    }
    /// Return true if we should compute a new heap limit. Only do so at the end of a mature GC
    fn generational_mem_stats_on_gc_end<VM: VMBinding>(&mut self, mmtk: &'static MMTK<VM>) -> bool {
        if !mmtk.plan.is_current_gc_nursery() {
            self.gc_end_live_pages = mmtk.plan.get_mature_reserved_pages();
            self.collection_pages = (self.gc_release_live_pages - self.gc_end_live_pages) as f64;
            trace!("collected pages = mature live at gc end {} - mature live at gc release {} = {}", self.gc_release_live_pages, self.gc_end_live_pages, self.collection_pages);
            true
        } else {
            false
        }
    }

    // Collect mem stats for non generational plans
    // * allocation = live pages at the start of GC - live pages at the end of last GC
    // * collection = live pages at the end of GC - live pages before release

    fn non_generational_mem_stats_on_gc_start<VM: VMBinding>(&mut self, mmtk: &'static MMTK<VM>) {
        self.allocation_pages = (mmtk.plan.get_reserved_pages() - self.gc_end_live_pages) as f64;
        trace!("allocated pages = used {} - live in last gc {} = {}", mmtk.plan.get_reserved_pages(), self.gc_end_live_pages, self.allocation_pages);
    }
    fn non_generational_mem_stats_on_gc_release<VM: VMBinding>(&mut self, mmtk: &'static MMTK<VM>) {
        self.gc_release_live_pages = mmtk.plan.get_reserved_pages();
        trace!("live before release = {}", self.gc_release_live_pages);
    }
    fn non_generational_mem_stats_on_gc_end<VM: VMBinding>(&mut self, mmtk: &'static MMTK<VM>) {
        self.gc_end_live_pages = mmtk.plan.get_reserved_pages();
        trace!("live pages = {}", self.gc_end_live_pages);
        self.collection_pages = (self.gc_release_live_pages - self.gc_end_live_pages) as f64;
        trace!("collected pages = live at gc end {} - live at gc release {} = {}", self.gc_release_live_pages, self.gc_end_live_pages, self.collection_pages);
    }
}

impl<VM: VMBinding> GCTriggerPolicy<VM> for MemBalancerTrigger {
    fn on_gc_start(&self, mmtk: &'static MMTK<VM>) {
        trace!("=== on_gc_start ===");
        self.access_stats(|stats| {
            stats.gc_start_time = Instant::now();
            stats.allocation_time += (stats.gc_start_time - stats.gc_end_time).as_secs_f64();
            trace!("gc_start = {:?}, allocation_time = {}", stats.gc_start_time, stats.allocation_time);

            if mmtk.plan.generational().is_some() {
                stats.generational_mem_stats_on_gc_start(mmtk);
            } else {
                stats.non_generational_mem_stats_on_gc_start(mmtk);
            }
        });
    }

    fn on_gc_release(&self, mmtk: &'static MMTK<VM>) {
        trace!("=== on_gc_release ===");
        self.access_stats(|stats| {
            if mmtk.plan.generational().is_some() {
                stats.generational_mem_stats_on_gc_release(mmtk);
            } else {
                stats.non_generational_mem_stats_on_gc_release(mmtk);
            }
        });
    }

    fn on_gc_end(&self, mmtk: &'static MMTK<VM>) {
        trace!("=== on_gc_end ===");
        self.access_stats(|stats| {
            stats.gc_end_time = Instant::now();
            stats.collection_time += (stats.gc_end_time - stats.gc_start_time).as_secs_f64();
            trace!("gc_end = {:?}, collection_time = {}", stats.gc_end_time, stats.collection_time);

            if mmtk.plan.generational().is_some() {
                if stats.generational_mem_stats_on_gc_end(mmtk) {
                    self.compute_new_heap_limit(mmtk.plan.get_mature_reserved_pages(), mmtk.plan.get_collection_reserved_pages(), stats);
                }
            } else {
                stats.non_generational_mem_stats_on_gc_end(mmtk);
                self.compute_new_heap_limit(mmtk.plan.get_reserved_pages(), mmtk.plan.get_collection_reserved_pages(), stats);
            }
        });
    }

    fn is_gc_required(
        &self,
        space_full: bool,
        space: Option<&dyn Space<VM>>,
        plan: &dyn Plan<VM = VM>,
    ) -> bool {
        // Let the plan decide
        plan.collection_required(space_full, space)
    }

    fn is_heap_full(&self, plan: &'static dyn Plan<VM = VM>) -> bool {
        // If reserved pages is larger than the current heap size, the heap is full.
        plan.get_reserved_pages() > self.current_heap_pages.load(Ordering::Relaxed)
    }

    fn get_heap_size_in_pages(&self) -> usize {
        self.current_heap_pages.load(Ordering::Relaxed)
    }

    fn can_heap_size_grow(&self) -> bool {
        self.current_heap_pages.load(Ordering::Relaxed) < self.max_heap_pages
    }
}
impl MemBalancerTrigger {
    fn new(min_heap_pages: usize, max_heap_pages: usize) -> Self {
        Self {
            min_heap_pages,
            max_heap_pages,
            // start with min heap
            current_heap_pages: AtomicUsize::new(min_heap_pages),
            stats: Atomic::new(Default::default())
        }
    }

    fn access_stats<F>(&self, mut f: F) where F: FnMut(&mut MemBalancerStats) {
        let mut stats = self.stats.load(Ordering::Relaxed);
        f(&mut stats);
        self.stats.store(stats, Ordering::Relaxed);
    }

    fn compute_new_heap_limit(&self, live: usize, extra_reserve: usize, stats: &mut MemBalancerStats) {
        trace!("compute new heap limit: {:?}", stats);
        const ALLOCATION_SMOOTH_FACTOR: f64 = 0.95;
        const COLLECTION_SMOOTH_FACTOR: f64 = 0.5;
        const TUNING_FACTOR: f64 = 0.2;

        let smooth = |prev, cur, factor| {
            if prev == NO_PREV {
                cur
            } else {
                prev * factor + cur * (1f64 - factor)
            }
        };
        let alloc_mem = smooth(stats.allocation_pages_prev, stats.allocation_pages, ALLOCATION_SMOOTH_FACTOR);
        let alloc_time = smooth(stats.allocation_time_prev, stats.allocation_time, ALLOCATION_SMOOTH_FACTOR);
        let gc_mem = smooth(stats.collection_pages_prev, stats.collection_pages, COLLECTION_SMOOTH_FACTOR);
        let gc_time = smooth(stats.collection_time_prev, stats.collection_time, COLLECTION_SMOOTH_FACTOR);
        trace!("after smoothing, alloc mem = {}, alloc_time = {}", alloc_mem, alloc_time);
        trace!("after smoothing, gc mem    = {}, gc_time    = {}", gc_mem, gc_time);

        stats.allocation_pages_prev = stats.allocation_pages;
        stats.allocation_pages = 0f64;
        stats.allocation_time_prev = stats.allocation_time;
        stats.allocation_time = 0f64;
        stats.collection_pages_prev = stats.collection_pages;
        stats.collection_pages = 0f64;
        stats.collection_time_prev = stats.collection_time;
        stats.collection_time = 0f64;

        let mut e = live as f64;
        e *= alloc_mem / alloc_time;
        e /= TUNING_FACTOR;
        e /= gc_mem / gc_time;
        e = e.sqrt();

        let optimal_heap = live + e as usize + extra_reserve;

        // The new heap size must be within min/max.
        let new_heap = optimal_heap.clamp(self.min_heap_pages, self.max_heap_pages);
        debug!(
            "MemBalander: new heap limit = {} pages (optimal = {}, clamped to [{}, {}])",
            new_heap, optimal_heap, self.min_heap_pages, self.max_heap_pages
        );
        self.current_heap_pages.store(new_heap, Ordering::Relaxed);
    }
}
