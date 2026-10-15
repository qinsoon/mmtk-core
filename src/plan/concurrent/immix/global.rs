use crate::plan::concurrent::global::ConcurrentPlan;
use crate::plan::concurrent::immix::gc_work::ConcurrentImmixGCWorkContext;
use crate::plan::concurrent::immix::gc_work::ConcurrentImmixSTWGCWorkContext;
use crate::plan::concurrent::Pause;
use crate::plan::global::BasePlan;
use crate::plan::global::CommonPlan;
use crate::plan::global::CreateGeneralPlanArgs;
use crate::plan::global::CreateSpecificPlanArgs;
use crate::plan::immix::mutator::ALLOCATOR_MAPPING;
use crate::plan::tracing::gc_work::weakref::VMProcessWeakRefs;
use crate::plan::AllocationSemantics;
use crate::plan::Plan;
use crate::plan::PlanConstraints;
use crate::policy::immix::defrag::StatsForDefrag;
use crate::policy::immix::ImmixSpaceArgs;
use crate::policy::immix::TRACE_KIND_DEFRAG;
use crate::policy::immix::TRACE_KIND_FAST;
use crate::policy::space::Space;
use crate::scheduler::gc_work::Release;
use crate::scheduler::gc_work::StopMutators;
use crate::scheduler::*;
use crate::util::alloc::allocators::AllocatorSelector;
use crate::util::copy::*;
use crate::util::heap::gc_trigger::SpaceStats;
use crate::util::heap::VMRequest;
use crate::util::metadata::log_bit::UnlogBitsOperation;
use crate::util::metadata::side_metadata::SideMetadataContext;
use crate::vm::ObjectModel;
use crate::vm::VMBinding;
use crate::MMTK;
use crate::{policy::immix::ImmixSpace, util::opaque_pointer::VMWorkerThread};
use std::sync::atomic::AtomicBool;

use atomic::Atomic;
use atomic::Ordering;
use enum_map::EnumMap;

use mmtk_macros::{HasSpaces, PlanTraceObject};

/// A concurrent Immix plan. The plan supports concurrent collection (strictly non-moving) and STW full heap collection (which may do defrag).
/// The concurrent GC consists of two STW pauses (initial mark and final mark) with concurrent marking in between,
/// followed by concurrent sweeping of the ImmixSpace after the final mark.
#[derive(HasSpaces, PlanTraceObject)]
pub struct ConcurrentImmix<VM: VMBinding> {
    #[post_scan]
    #[space]
    #[copy_semantics(CopySemantics::DefaultCopy)]
    pub immix_space: ImmixSpace<VM>,
    #[parent]
    pub common: CommonPlan<VM>,
    last_gc_was_defrag: AtomicBool,
    current_pause: Atomic<Option<Pause>>,
    previous_pause: Atomic<Option<Pause>>,
    should_do_full_gc: AtomicBool,
    concurrent_marking_active: AtomicBool,
    unfinished_concurrent_marking: AtomicBool,
    /// `true` from the `Release` stage of a `FinalMark` pause until the ImmixSpace has been
    /// swept concurrently.
    concurrent_sweeping_active: AtomicBool,
    /// A mutator found the heap full during concurrent sweeping.  See `collection_required`.
    heap_full_during_sweeping: AtomicBool,
}

/// The plan constraints for the concurrent immix plan.
pub const CONCURRENT_IMMIX_CONSTRAINTS: PlanConstraints = PlanConstraints {
    // If we disable moving in Immix, this is a non-moving plan.
    moves_objects: !cfg!(feature = "immix_non_moving"),
    // Max immix object size is half of a block.
    max_non_los_default_alloc_bytes: crate::policy::immix::MAX_IMMIX_OBJECT_SIZE,
    needs_prepare_mutator: true,
    barrier: crate::BarrierSelector::SATBBarrier,
    needs_log_bit: true,
    ..PlanConstraints::default()
};

impl<VM: VMBinding> Plan for ConcurrentImmix<VM> {
    fn collection_required(&self, space_full: bool, _space: Option<SpaceStats<Self::VM>>) -> bool {
        if self.base().collection_required(self, space_full) {
            if self.concurrent_sweeping_in_progress() {
                // The heap still holds the garbage being swept.  The pause waits for sweeping to
                // finish, and `schedule_collection` checks again whether the heap is full.
                self.heap_full_during_sweeping.store(true, Ordering::SeqCst);
            } else {
                self.should_do_full_gc.store(true, Ordering::Release);
                info!("Triggering full GC");
            }
            return true;
        }

        // Check stw for final mark
        let concurrent_marking_in_progress = self.concurrent_marking_in_progress();
        if concurrent_marking_in_progress
            && self.common.base.scheduler.work_buckets[WorkBucketStage::Concurrent].is_drained()
        {
            // After the Concurrent bucket is drained during concurrent marking,
            // we trigger the FinalMark pause at the next poll() site (here).
            // FIXME: Immediately trigger FinalMark when the Concurrent bucket is drained.
            return true;
        }

        // Check stw for initial mark

        // If concurrent marking is disbled, no need to check further.
        if self.concurrent_marking_is_disabled() {
            return false;
        }

        // Do not start the next concurrent marking cycle before the last one finished sweeping.
        // Its line and block states are not ready for marking until then.
        if self.concurrent_sweeping_in_progress() {
            return false;
        }

        let threshold = self.get_total_pages() >> 1;
        let used_pages_after_last_gc = self.common.base.global_state.get_used_pages_after_last_gc();
        let used_pages_now = self.get_used_pages();
        let allocated = used_pages_now.saturating_sub(used_pages_after_last_gc);
        if !concurrent_marking_in_progress && allocated > threshold {
            info!("Allocated {allocated} pages since last GC ({used_pages_now} - {used_pages_after_last_gc} > {threshold}): Do concurrent marking");
            debug_assert!(
                self.common.base.scheduler.work_buckets[WorkBucketStage::Concurrent].is_empty()
            );
            debug_assert!(!self.concurrent_marking_in_progress());
            debug_assert_ne!(self.previous_pause(), Some(Pause::InitialMark));
            return true;
        }

        false
    }

    fn last_collection_was_exhaustive(&self) -> bool {
        self.immix_space
            .is_last_gc_exhaustive(self.last_gc_was_defrag.load(Ordering::Relaxed))
    }

    fn constraints(&self) -> &'static PlanConstraints {
        &CONCURRENT_IMMIX_CONSTRAINTS
    }

    fn create_copy_config(&'static self) -> CopyConfig<Self::VM> {
        use enum_map::enum_map;
        CopyConfig {
            copy_mapping: enum_map! {
                CopySemantics::DefaultCopy => CopySelector::Immix(0),
                _ => CopySelector::Unused,
            },
            space_mapping: vec![(CopySelector::Immix(0), &self.immix_space)],
            constraints: &CONCURRENT_IMMIX_CONSTRAINTS,
        }
    }

    fn schedule_collection(&'static self, scheduler: &GCWorkScheduler<VM>) {
        // If concurrent marking is disabled, force a full GC.
        // Though we have checked in collection_required to not trigger a concurrent GC, it is still possible
        // that a GC is triggered without going through collection_required, e.g. a user triggered GC, or a GC trigger
        // implemented at the binding side without calling collection_required.
        // In those cases, we also want to force a full GC.
        if self.concurrent_marking_is_disabled() {
            self.should_do_full_gc.store(true, Ordering::SeqCst);
        }

        // Concurrent sweeping has finished before this pause (see `on_pause_start`).  If a mutator
        // found the heap full during sweeping, do a full GC only if the heap is still full.
        // Otherwise, start the next concurrent cycle.
        if self.heap_full_during_sweeping.swap(false, Ordering::SeqCst) {
            if self.base().gc_trigger.is_heap_full()
                || self.base().global_state.is_emergency_collection()
            {
                info!("Triggering full GC");
                self.should_do_full_gc.store(true, Ordering::SeqCst);
            } else {
                info!("The heap is no longer full after concurrent sweeping");
            }
        }

        let pause = if self.concurrent_marking_in_progress() {
            // FIXME: Currently it is unsafe to bypass `FinalMark` and go directly from `InitialMark` to `Full`.
            // It is related to defragmentation.  See https://github.com/mmtk/mmtk-core/issues/1357 for more details.
            // We currently force `FinalMark` to happen if the last pause is `InitialMark`.
            Pause::FinalMark
        } else if self.should_do_full_gc.load(Ordering::SeqCst)
            // For user-triggered GCs, we don't want a simple initial pause which reclaims nothing.
            // We do a full STW collection for user triggered collection instead.
            || self.base().global_state.is_user_triggered_collection()
        {
            Pause::Full
        } else {
            Pause::InitialMark
        };

        self.current_pause.store(Some(pause), Ordering::SeqCst);

        probe!(mmtk, concurrent_pause_determined, pause as usize);

        match pause {
            Pause::Full => {
                // Ref closure buckets is disabled by initial mark, and needs to be re-enabled for full GC before
                // we reuse the normal Immix scheduling.
                self.set_ref_closure_buckets_enabled(true);
                crate::plan::immix::global::Immix::schedule_immix_full_heap_collection::<
                    ConcurrentImmix<VM>,
                    ConcurrentImmixSTWGCWorkContext<VM, TRACE_KIND_FAST>,
                    ConcurrentImmixSTWGCWorkContext<VM, TRACE_KIND_DEFRAG>,
                >(self, &self.immix_space, scheduler);
            }
            Pause::InitialMark => self.schedule_concurrent_marking_initial_pause(scheduler),
            Pause::FinalMark => self.schedule_concurrent_marking_final_pause(scheduler),
            Pause::RefCount => unreachable!(),
        }
    }

    fn get_allocator_mapping(&self) -> &'static EnumMap<AllocationSemantics, AllocatorSelector> {
        &ALLOCATOR_MAPPING
    }

    fn prepare(&mut self, tls: VMWorkerThread) {
        let pause = self.current_pause().unwrap();
        match pause {
            Pause::Full => {
                self.common.prepare(tls, true);
                self.immix_space.prepare(
                    true,
                    Some(StatsForDefrag::new(self)),
                    // Ignore unlog bits in full GCs because unlog bits should be all 0.
                    UnlogBitsOperation::NoOp,
                );
            }
            Pause::InitialMark => {
                self.immix_space.prepare(
                    true,
                    Some(StatsForDefrag::new(self)),
                    // Bulk set log bits so SATB barrier will be triggered on the existing objects.
                    UnlogBitsOperation::BulkSet,
                );

                self.common.prepare(tls, true);
                // Bulk set log bits so SATB barrier will be triggered on the existing objects.
                self.common
                    .schedule_unlog_bits_op(UnlogBitsOperation::BulkSet);
            }
            Pause::FinalMark => (),
            Pause::RefCount => unreachable!(),
        }
    }

    fn release(&mut self, tls: VMWorkerThread) {
        let pause = self.current_pause().unwrap();
        match pause {
            Pause::InitialMark => (),
            Pause::Full | Pause::FinalMark => {
                // Bulk clear log bits so SATB barrier will not be triggered.
                let unlog_bits_op = UnlogBitsOperation::BulkClear;
                if pause == Pause::FinalMark && self.should_sweep_concurrently() {
                    // Clearing log bits concurrently is fine. Mutators drop the SATB entries
                    // recorded when hitting the slow path after concurrent marking.
                    self.release_for_concurrent_sweeping(unlog_bits_op);
                } else {
                    self.immix_space.release(true, unlog_bits_op);
                }

                self.common.release(tls, true);

                if pause == Pause::FinalMark {
                    // Bulk clear log bits so SATB barrier will not be triggered.
                    self.common
                        .schedule_unlog_bits_op(UnlogBitsOperation::BulkClear);
                } else {
                    // Full pauses didn't set unlog bits in the first place,
                    // so there is no need to clear them.
                    // TODO: Currently InitialMark must be followed by a FinalMark.
                    // If we allow upgrading a concurrent GC to a full STW GC,
                    // we will need to clear the unlog bits at an appropriate place.
                }
            }
            Pause::RefCount => unreachable!(),
        }
    }

    fn on_pause_end(&mut self, mmtk: &'static MMTK<VM>, _tls: VMWorkerThread) {
        self.last_gc_was_defrag
            .store(self.immix_space.end_of_gc(), Ordering::Relaxed);

        let pause = self.current_pause().unwrap();
        if pause == Pause::InitialMark {
            self.set_concurrent_marking_state(true);
        }
        if pause == Pause::FinalMark {
            // All marking must be finished by the end of `FinalMark`.  Work left in the
            // `Concurrent` bucket would be traced after the heap has been released.
            let concurrent_bucket = &mmtk.scheduler.work_buckets[WorkBucketStage::Concurrent];
            assert!(
                concurrent_bucket.is_empty(),
                "Concurrent bucket is not empty at the end of FinalMark ({} packets)",
                concurrent_bucket.len()
            );
        }
        self.previous_pause.store(Some(pause), Ordering::SeqCst);
        self.current_pause.store(None, Ordering::SeqCst);
        if pause != Pause::FinalMark {
            self.should_do_full_gc.store(false, Ordering::SeqCst);
        } else {
            // FIXME: Currently it is unsafe to trigger full GC during concurrent marking.
            // See `Self::schedule_collection`.
            // We keep the value of `self.should_do_full_gc` so that if full GC is triggered,
            // the next GC will be full GC.
        }

        // Every pause ends a GC cycle, except `InitialMark`, which is followed by concurrent
        // marking and a `FinalMark` pause before the cycle ends, and a `FinalMark` followed by
        // concurrent sweeping, which ends the cycle when it finishes.
        if pause != Pause::InitialMark && !self.concurrent_sweeping_in_progress() {
            mmtk.gc_trigger.policy.on_gc_end(mmtk);
        }

        info!("{:?} end", pause);
    }

    fn current_gc_may_move_object(&self) -> bool {
        self.immix_space.in_defrag()
    }

    fn get_collection_reserved_pages(&self) -> usize {
        self.immix_space.defrag_headroom_pages()
    }

    fn get_used_pages(&self) -> usize {
        self.immix_space.reserved_pages() + self.common.get_used_pages()
    }

    fn base(&self) -> &BasePlan<VM> {
        &self.common.base
    }

    fn base_mut(&mut self) -> &mut BasePlan<Self::VM> {
        &mut self.common.base
    }

    fn common(&self) -> &CommonPlan<VM> {
        &self.common
    }

    fn on_pause_start(&self, mmtk: &'static MMTK<VM>) {
        use crate::vm::ActivePlan;

        // A pause requested during concurrent sweeping waits for GC workers to drain the
        // `Concurrent` bucket (see `on_concurrent_work_interrupted`), and the last sweeping packet
        // has called `on_concurrent_sweeping_finished`.
        assert!(
            !self.concurrent_sweeping_in_progress(),
            "A pause started during concurrent sweeping"
        );

        let pause = self.current_pause().unwrap();
        if pause != Pause::FinalMark {
            // If the last cycle swept concurrently, nothing has disabled the `Concurrent` bucket
            // since then.  Disable it, or the concurrent marking packets that `InitialMark`
            // creates would be executed in the pause.  The scheduler enables it at the end of the
            // pause if it holds work.
            let concurrent_bucket = &mmtk.scheduler.work_buckets[WorkBucketStage::Concurrent];
            debug_assert!(concurrent_bucket.is_empty());
            concurrent_bucket.set_enabled(false);
        }
        match pause {
            Pause::Full => {
                self.set_concurrent_marking_state(false);
            }
            Pause::InitialMark => {
                debug_assert!(
                    !self.concurrent_marking_in_progress(),
                    "prev pause: {:?}",
                    self.previous_pause().unwrap()
                );
            }
            Pause::FinalMark => {
                debug_assert!(self.concurrent_marking_in_progress());
                // Flush barrier buffers
                for mutator in <VM as VMBinding>::VMActivePlan::mutators() {
                    mutator.barrier.flush();
                }
                self.set_concurrent_marking_state(false);
            }
            Pause::RefCount => unreachable!(),
        }

        // Every pause starts a new GC cycle, except `FinalMark`, which continues the cycle
        // started by the preceding `InitialMark` pause.
        if pause != Pause::FinalMark {
            mmtk.gc_trigger.policy.on_gc_start(mmtk);
        }

        // If we have unfinished concurrent marking work, do it here.
        if self.unfinished_concurrent_marking.load(Ordering::SeqCst) {
            info!(
                "Concurrent marking was interrupted. Moving remaining work to STW closure bucket."
            );
            // We have unfinihsed concurrent marking work, so this pause has to be the final mark pause.
            // If we want to allow full pause to interrupte concurrent marking, the unfinished work needs to be dropped.
            assert!(pause == Pause::FinalMark);
            let leftover_concurrent_work =
                mmtk.scheduler.work_buckets[WorkBucketStage::Concurrent].drain_all_packets();
            mmtk.scheduler.work_buckets[WorkBucketStage::FinishConcurrentWork]
                .bulk_add(leftover_concurrent_work);
            self.unfinished_concurrent_marking
                .store(false, Ordering::SeqCst);
        }

        info!("{:?} start", pause);
    }

    fn concurrent(&self) -> Option<&dyn ConcurrentPlan<VM = VM>> {
        Some(self)
    }
}

impl<VM: VMBinding> ConcurrentImmix<VM> {
    pub fn new(args: CreateGeneralPlanArgs<VM>) -> Self {
        if *args.options.concurrent_immix_disable_concurrent_marking {
            warn!("Option 'concurrent_immix_disable_concurrent_marking' is set to true. Concurrent marking is disabled for ConcurrentImmix. This will make ConcurrentImmix behave exactly like full heap Immix.");
        }

        let spec = crate::util::metadata::extract_side_metadata(&[
            *VM::VMObjectModel::GLOBAL_LOG_BIT_SPEC,
        ]);

        let mut plan_args = CreateSpecificPlanArgs {
            global_args: args,
            constraints: &CONCURRENT_IMMIX_CONSTRAINTS,
            global_side_metadata_specs: SideMetadataContext::new_global_specs(&spec),
        };

        let immix_args = ImmixSpaceArgs {
            mixed_age: false,
            never_move_objects: false,
        };

        // These buckets are not used in an Immix plan. We can simply disable them.
        // TODO: We should be more systmatic on this, and disable unnecessary buckets for other plans as well.
        let scheduler = &plan_args.global_args.scheduler;
        scheduler.work_buckets[WorkBucketStage::VMRefForwarding].set_enabled(false);
        scheduler.work_buckets[WorkBucketStage::CalculateForwarding].set_enabled(false);
        scheduler.work_buckets[WorkBucketStage::SecondRoots].set_enabled(false);
        scheduler.work_buckets[WorkBucketStage::RefForwarding].set_enabled(false);
        scheduler.work_buckets[WorkBucketStage::FinalizableForwarding].set_enabled(false);
        scheduler.work_buckets[WorkBucketStage::Compact].set_enabled(false);

        ConcurrentImmix {
            immix_space: ImmixSpace::new(
                plan_args.get_normal_space_args("immix", true, false, VMRequest::discontiguous()),
                immix_args,
            ),
            common: CommonPlan::new(plan_args),
            last_gc_was_defrag: AtomicBool::new(false),
            current_pause: Atomic::new(None),
            previous_pause: Atomic::new(None),
            should_do_full_gc: AtomicBool::new(false),
            concurrent_marking_active: AtomicBool::new(false),
            unfinished_concurrent_marking: AtomicBool::new(false),
            concurrent_sweeping_active: AtomicBool::new(false),
            heap_full_during_sweeping: AtomicBool::new(false),
        }
    }

    fn set_ref_closure_buckets_enabled(&self, do_closure: bool) {
        let scheduler = &self.common.base.scheduler;
        scheduler.work_buckets[WorkBucketStage::VMRefClosure].set_enabled(do_closure);
        scheduler.work_buckets[WorkBucketStage::WeakRefClosure].set_enabled(do_closure);
        scheduler.work_buckets[WorkBucketStage::FinalRefClosure].set_enabled(do_closure);
        scheduler.work_buckets[WorkBucketStage::SoftRefClosure].set_enabled(do_closure);
        scheduler.work_buckets[WorkBucketStage::PhantomRefClosure].set_enabled(do_closure);
    }

    pub(crate) fn schedule_concurrent_marking_initial_pause(
        &'static self,
        scheduler: &GCWorkScheduler<VM>,
    ) {
        use crate::scheduler::gc_work::Prepare;

        self.set_ref_closure_buckets_enabled(false);

        scheduler.work_buckets[WorkBucketStage::Unconstrained]
            .add(StopMutators::<ConcurrentImmixGCWorkContext<VM>>::new());
        scheduler.work_buckets[WorkBucketStage::Prepare]
            .add(Prepare::<ConcurrentImmixGCWorkContext<VM>>::new(self));
    }

    fn schedule_concurrent_marking_final_pause(&'static self, scheduler: &GCWorkScheduler<VM>) {
        self.set_ref_closure_buckets_enabled(true);

        // Skip root scanning in the final mark
        scheduler.work_buckets[WorkBucketStage::Unconstrained]
            .add(StopMutators::<ConcurrentImmixGCWorkContext<VM>>::new_no_scan_roots());

        scheduler.work_buckets[WorkBucketStage::Release]
            .add(Release::<ConcurrentImmixGCWorkContext<VM>>::new(self));

        // Sanity
        #[cfg(feature = "sanity")]
        {
            use crate::util::sanity::sanity_checker::ScheduleSanityGC;
            scheduler.work_buckets[WorkBucketStage::Final].add(ScheduleSanityGC::<Self>::new(self));
        }

        // Deal with weak ref and finalizers
        // TODO: Check against schedule_common_work and see if we are still missing any work packet
        type RefTracePolicy<VM> =
            crate::plan::tracing::PlanTrace<ConcurrentImmix<VM>, TRACE_KIND_FAST>;
        // Reference processing
        if !*self.base().options.no_reference_types {
            use crate::util::reference_processor::{
                PhantomRefProcessing, SoftRefProcessing, WeakRefProcessing,
            };
            scheduler.work_buckets[WorkBucketStage::SoftRefClosure]
                .add(SoftRefProcessing::<RefTracePolicy<VM>>::new());
            scheduler.work_buckets[WorkBucketStage::WeakRefClosure]
                .add(WeakRefProcessing::<VM>::new());
            scheduler.work_buckets[WorkBucketStage::PhantomRefClosure]
                .add(PhantomRefProcessing::<VM>::new());

            use crate::util::reference_processor::RefEnqueue;
            scheduler.work_buckets[WorkBucketStage::Release].add(RefEnqueue::<VM>::new());
        }

        // Finalization
        if !*self.base().options.no_finalizer {
            use crate::util::finalizable_processor::Finalization;
            // finalization
            scheduler.work_buckets[WorkBucketStage::FinalRefClosure]
                .add(Finalization::<RefTracePolicy<VM>>::new());
        }

        // VM-specific weak ref processing
        // Note that ConcurrentImmix does not have a separate forwarding stage,
        // so we don't schedule the `VMForwardWeakRefs` work packet.
        scheduler.work_buckets[WorkBucketStage::VMRefClosure]
            .set_sentinel(Box::new(VMProcessWeakRefs::<RefTracePolicy<VM>>::new()));
    }

    pub fn concurrent_marking_in_progress(&self) -> bool {
        self.concurrent_marking_active.load(Ordering::Acquire)
    }

    fn set_concurrent_marking_state(&self, active: bool) {
        use crate::plan::global::HasSpaces;

        // Tell the spaces to allocate new objects as live
        let allocate_object_as_live = active;
        self.for_each_space(&mut |space: &dyn Space<VM>| {
            space.set_allocate_as_live(allocate_object_as_live);
        });

        // Store the state.
        self.concurrent_marking_active
            .store(active, Ordering::SeqCst);

        // We also set SATB barrier as active -- this is done in Mutator prepare/release.
    }

    pub(super) fn is_concurrent_marking_active(&self) -> bool {
        self.concurrent_marking_active.load(Ordering::SeqCst)
    }

    pub fn concurrent_sweeping_in_progress(&self) -> bool {
        self.concurrent_sweeping_active.load(Ordering::Acquire)
    }

    /// Return `true` if the current `FinalMark` pause should leave sweeping the ImmixSpace to
    /// GC workers running concurrently with mutators.
    fn should_sweep_concurrently(&self) -> bool {
        let global_state = &self.base().global_state;
        !*self
            .base()
            .options
            .concurrent_immix_disable_concurrent_sweeping
            // Sweeping keeps new blocks alive by their line marks, which block-only Immix ignores.
            && !crate::policy::immix::BLOCK_ONLY
            // Sweeping updates the VO bits of the blocks it sweeps, but mutators would be setting
            // VO bits of the objects they allocate at the same time.
            && !cfg!(feature = "vo_bit")
            // If the heap is full, or the user or an emergency asked for memory, mutators need the
            // memory as soon as they resume.  Sweep in the pause.
            && !self.should_do_full_gc.load(Ordering::SeqCst)
            && !global_state.is_user_triggered_collection()
            && !global_state.is_emergency_collection()
    }

    /// Called in the `Release` stage of a `FinalMark` pause.  Defer the work packets that sweep
    /// the ImmixSpace to the `Concurrent` bucket, so that GC workers execute them after the pause,
    /// concurrently with mutators.
    fn release_for_concurrent_sweeping(&mut self, unlog_bits_op: UnlogBitsOperation) {
        let sweep_work = self.immix_space.release_for_concurrent_sweeping(
            unlog_bits_op,
            Box::new(|mmtk: &'static MMTK<VM>| {
                let plan = mmtk.get_plan().downcast_ref::<Self>().unwrap();
                plan.on_concurrent_sweeping_finished(mmtk);
            }),
        );
        if sweep_work.is_empty() {
            return;
        }
        // See `ImmixSpace::release_for_concurrent_sweeping`.  Blocks that mutators acquire before
        // the sweeping packets reach them must look live to the sweeping packets.
        self.immix_space.set_allocate_as_live(true);
        self.concurrent_sweeping_active
            .store(true, Ordering::SeqCst);
        // The bucket is open during `FinalMark` to process the SATB buffers flushed at its start.
        // Deferred packets wait until the scheduler activates them at the end of the pause.
        self.common.base.scheduler.work_buckets[WorkBucketStage::Concurrent]
            .bulk_add_deferred(sweep_work);
        info!("Concurrent sweeping scheduled");
    }

    /// Called by the last sweeping packet, concurrently with mutators, once the ImmixSpace has been
    /// swept.  Ends the GC cycle of the last `FinalMark` pause.
    fn on_concurrent_sweeping_finished(&self, mmtk: &'static MMTK<VM>) {
        debug_assert!(self.concurrent_sweeping_in_progress());
        self.immix_space.set_allocate_as_live(false);
        self.concurrent_sweeping_active
            .store(false, Ordering::SeqCst);

        // The used pages recorded at the end of `FinalMark` still included the garbage in the
        // ImmixSpace.  Don't count it as allocation when deciding the next `InitialMark`.
        let global_state = &self.base().global_state;
        let released_pages = self.immix_space.get_pages_released_by_sweeping();
        global_state.set_used_pages_after_last_gc(
            global_state
                .get_used_pages_after_last_gc()
                .saturating_sub(released_pages),
        );

        mmtk.gc_trigger.policy.on_gc_end(mmtk);

        info!("Concurrent sweeping finished ({released_pages} pages released)");
    }

    fn previous_pause(&self) -> Option<Pause> {
        self.previous_pause.load(Ordering::SeqCst)
    }

    fn concurrent_marking_is_disabled(&self) -> bool {
        *self
            .base()
            .options
            .concurrent_immix_disable_concurrent_marking
    }
}

impl<VM: VMBinding> ConcurrentPlan for ConcurrentImmix<VM> {
    fn current_pause(&self) -> Option<Pause> {
        self.current_pause.load(Ordering::SeqCst)
    }

    fn concurrent_work_in_progress(&self) -> bool {
        self.concurrent_marking_in_progress()
    }

    fn on_concurrent_work_interrupted(&self) {
        if !self.concurrent_marking_in_progress() {
            // The concurrent work is sweeping.  Leave the `Concurrent` bucket enabled so that GC
            // workers finish sweeping before the requested pause starts.  The pause must not
            // start before sweeping finishes (see `ImmixSpace::release_for_concurrent_sweeping`),
            // and finishing it while mutators still run keeps it out of the pause.
            return;
        }
        assert!(!self.unfinished_concurrent_marking.load(Ordering::SeqCst));
        // A pause is requested when we are doing concurrent marking.
        // Set concurrent bucket as disabled now. Later (during collection scheduling),
        // we will move all the remaining work to a STW bucket and continue.
        // This preserves all marking progress already made; nothing is reset or re-traced.
        self.common.base.scheduler.work_buckets[WorkBucketStage::Concurrent].set_enabled(false);
        self.unfinished_concurrent_marking
            .store(true, Ordering::SeqCst);
    }

    fn on_concurrent_work_drained(&self) {
        // Concurrent marking always ends in a `FinalMark` pause. Nothing to do here.
        if self.concurrent_marking_in_progress() {
            return;
        }
        // Otherwise, the concurrent work was sweeping, and no pause follows it.
        debug_assert!(
            !self.concurrent_sweeping_in_progress(),
            "Concurrent sweeping is not finished with every GC worker parked and no work left"
        );
        self.finish_concurrent_work_without_pause();
    }
}
