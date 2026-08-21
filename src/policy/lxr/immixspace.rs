//! LXR's ref-counting flavor of [`ImmixSpace`].

use crate::plan::tracing::OptionObjectQueue;
use crate::plan::ObjectQueue;
use crate::plan::Pause;
use crate::policy::gc_work::{PolicyTraceObject, TraceKind};
use crate::policy::immix::block::Block;
use crate::policy::immix::line::{Line, RCArray};
use crate::policy::immix::{ImmixSpace, ImmixSpaceArgs, ImmixSpaceExt};
use crate::policy::lxr::block::LXRBlockExt;
use crate::policy::sft::GCWorkerMutRef;
use crate::policy::sft::SFT;
use crate::policy::sft_map::SFTMap;
use crate::policy::space::{CommonSpace, PlanCreateSpaceArgs, Space};
use crate::scheduler::gc_work::PrepareCollector;
use crate::scheduler::{GCWorker, GCWorkScheduler};
use crate::util::alloc::allocator::AllocationOptions;
use crate::util::copy::CopySemantics;
use crate::util::heap::PageResource;
use crate::util::object_enum::ObjectEnumerator;
use crate::util::linear_scan::{Region, UnstraddlableRegion};
use crate::util::object_forwarding;
use crate::util::opaque_pointer::VMThread;
use crate::util::rc::RefCountHelper;
use crate::util::{Address, ObjectReference};
use crate::vm::VMBinding;
use std::sync::OnceLock;

/// Whether RC-mode mature-space evacuation is compiled in.
const LXR_MATURE_EVACUATION: bool = !cfg!(feature = "lxr_no_mature_evac");

/// Plan-level hooks invoked by [`LXRSpace`] during mutator allocation.
/// Default impls are no-ops; LXR's `BlockAllocation` provides the concrete implementation.
pub trait ImmixHooks<VM: VMBinding>: Send + Sync {
    /// Called after a fresh clean block is acquired. `copy` distinguishes
    /// mutator vs. GC-copy allocation. The hook owns any plan-specific
    /// per-block bookkeeping (e.g. nursery list, mark-table init).
    fn on_clean_block_acquired(&self, _block: Block, _copy: bool) {}
    /// Called after a reusable block is handed out to a mutator.
    fn on_reusable_block_acquired(&self, _block: Block, _copy: bool) {}
    /// Whether tracing is in progress; consulted on the mutator
    /// reused-line fast path so newly handed-out lines can be marked.
    fn cm_in_progress_or_final_mark(&self) -> bool {
        false
    }
}

/// LXR's ref-counting flavor of [`ImmixSpace`]. Wraps a generic, tracing-only `ImmixSpace` and
/// layers reference-counting semantics (mark-and-RC liveness, RC-based hole search, mature-object
/// evacuation with RC transfer) on top, reusing the inner space's block/chunk/page-resource
/// machinery rather than duplicating it.
pub struct LXRSpace<VM: VMBinding> {
    immix: ImmixSpace<VM>,
    hooks: OnceLock<&'static dyn ImmixHooks<VM>>,
    pub is_end_of_satb_or_full_gc: bool,
    pub rc: RefCountHelper<VM>,
}

unsafe impl<VM: VMBinding> Sync for LXRSpace<VM> {}

impl<VM: VMBinding> SFT for LXRSpace<VM> {
    fn name(&self) -> &'static str {
        self.get_name()
    }

    fn get_forwarded_object(&self, object: ObjectReference) -> Option<ObjectReference> {
        if object_forwarding::is_forwarded::<VM>(object) {
            Some(object_forwarding::read_forwarding_pointer::<VM>(object))
        } else {
            None
        }
    }

    fn is_live(&self, object: ObjectReference) -> bool {
        if self.is_end_of_satb_or_full_gc {
            if self.is_marked(object) {
                let block = Block::containing(object);
                if block.is_defrag_source() {
                    if object_forwarding::is_forwarded::<VM>(object) {
                        let forwarded = object_forwarding::read_forwarding_pointer::<VM>(object);
                        return self.is_marked(forwarded) && self.rc.count(forwarded) > 0;
                    } else {
                        return false;
                    }
                }
                return self.rc.count(object) > 0;
            } else if object_forwarding::is_forwarded::<VM>(object) {
                let forwarded = object_forwarding::read_forwarding_pointer::<VM>(object);
                debug_assert!(
                    forwarded.to_raw_address().is_mapped(),
                    "Invalid forwarded object: {:?} -> {:?}",
                    object,
                    forwarded
                );
                return self.is_marked(forwarded) && self.rc.count(forwarded) > 0;
            } else {
                return false;
            }
        }
        self.rc.count(object) > 0 || object_forwarding::is_forwarded::<VM>(object)
    }

    fn is_reachable(&self, object: ObjectReference) -> bool {
        if object_forwarding::is_forwarded::<VM>(object) {
            let forwarded = object_forwarding::read_forwarding_pointer::<VM>(object);
            return self.is_marked(forwarded) && self.rc.count(forwarded) > 0;
        }
        self.is_marked(object) && self.rc.count(object) > 0
    }
    #[cfg(feature = "object_pinning")]
    fn pin_object(&self, object: ObjectReference) -> bool {
        self.immix.pin_object(object)
    }
    #[cfg(feature = "object_pinning")]
    fn unpin_object(&self, object: ObjectReference) -> bool {
        self.immix.unpin_object(object)
    }
    #[cfg(feature = "object_pinning")]
    fn is_object_pinned(&self, object: ObjectReference) -> bool {
        self.immix.is_object_pinned(object)
    }
    fn is_movable(&self) -> bool {
        self.immix.is_movable()
    }
    #[cfg(feature = "sanity")]
    fn is_sane(&self) -> bool {
        true
    }
    fn initialize_object_metadata(&self, object: ObjectReference, bytes: usize) {
        self.immix.initialize_object_metadata(object, bytes)
    }
    #[cfg(feature = "vo_bit")]
    fn is_mmtk_object(&self, addr: Address) -> Option<ObjectReference> {
        self.immix.is_mmtk_object(addr)
    }
    #[cfg(feature = "vo_bit")]
    fn find_object_from_internal_pointer(
        &self,
        ptr: Address,
        max_search_bytes: usize,
    ) -> Option<ObjectReference> {
        self.immix.find_object_from_internal_pointer(ptr, max_search_bytes)
    }
    fn sft_trace_object(
        &self,
        _queue: &mut OptionObjectQueue,
        _object: ObjectReference,
        _worker: GCWorkerMutRef,
    ) -> ObjectReference {
        panic!("We do not use SFT to trace objects for Immix. sft_trace_object() cannot be used.")
    }

    fn debug_print_object_info(&self, object: ObjectReference) {
        println!("marked  = {}", self.is_marked(object));
        // The line mark table isn't mapped under RC (LXR tracks liveness via block state and
        // reference counts instead), so we don't print it here.
        println!(
            "block state = {:?}",
            Block::from_unaligned_address(object.to_raw_address()).get_state()
        );
        object_forwarding::debug_print_object_forwarding_info::<VM>(object);
        self.immix.common().debug_print_object_global_info(object);
    }
}

impl<VM: VMBinding> Space<VM> for LXRSpace<VM> {
    fn as_space(&self) -> &dyn Space<VM> {
        self
    }
    fn as_sft(&self) -> &(dyn SFT + Sync + 'static) {
        self
    }
    fn get_page_resource(&self) -> &dyn PageResource<VM> {
        self.immix.get_page_resource()
    }
    fn maybe_get_page_resource_mut(&mut self) -> Option<&mut dyn PageResource<VM>> {
        self.immix.maybe_get_page_resource_mut()
    }
    fn common(&self) -> &CommonSpace<VM> {
        self.immix.common()
    }
    fn initialize_sft(&self, sft_map: &mut dyn SFTMap) {
        // Register *this* wrapper (not the inner `ImmixSpace`) as the SFT for this space, so
        // liveness/tracing dispatch goes through the RC-flavored impls above.
        self.common().initialize_sft(self.as_sft(), sft_map)
    }
    fn release_multiple_pages(&mut self, _start: Address) {
        panic!("immixspace only releases pages enmasse")
    }
    fn set_copy_for_sft_trace(&mut self, _semantics: Option<CopySemantics>) {
        panic!("We do not use SFT to trace objects for Immix. set_copy_context() cannot be used.")
    }
    fn enumerate_objects(&self, enumerator: &mut dyn ObjectEnumerator) {
        self.immix.enumerate_objects(enumerator)
    }
    fn clear_side_log_bits(&self) {
        self.immix.clear_side_log_bits()
    }
    fn set_side_log_bits(&self) {
        self.immix.set_side_log_bits()
    }
}

impl<VM: VMBinding> PolicyTraceObject<VM> for LXRSpace<VM> {
    fn trace_object<Q: ObjectQueue, const KIND: TraceKind>(
        &self,
        queue: &mut Q,
        object: ObjectReference,
        _copy: Option<CopySemantics>,
        _worker: &mut GCWorker<VM>,
    ) -> ObjectReference {
        // LXR drives all of its real tracing directly (via `rc_trace_object` and friends from
        // `plan::lxr::gc_work`), bypassing the generic `PolicyTraceObject` dispatch. This impl
        // only exists to satisfy `#[derive(PlanTraceObject)]`'s trait bound on the `#[space]`
        // field, and mirrors the plain, non-evacuating tracing fast path.
        self.trace_object_without_moving_rc(queue, object)
    }

    fn post_scan_object(&self, _object: ObjectReference) {
        // LXR does not use line-mark-time scanning hooks.
    }

    fn may_move_objects<const KIND: TraceKind>() -> bool {
        // LXR only moves objects via its own mature-evacuation path, never through the generic
        // PolicyTraceObject dispatch.
        false
    }
}

impl<VM: VMBinding> ImmixSpaceExt<VM> for LXRSpace<VM> {
    fn get_clean_block(
        &self,
        tls: VMThread,
        copy: bool,
        alloc_options: AllocationOptions,
    ) -> Option<Block> {
        let block = self.immix.acquire_clean_block_raw(tls, alloc_options)?;
        if self.immix.in_defrag() {
            self.immix.notify_new_clean_block(copy);
        }
        if let Some(hooks) = self.hooks() {
            hooks.on_clean_block_acquired(block, copy);
        }
        block.init(copy, false, true);
        self.immix.chunk_map.set_allocated(block.chunk(), true);
        Some(block)
    }

    fn get_reusable_block(&self, copy: bool) -> Option<Block> {
        if crate::policy::immix::BLOCK_ONLY {
            return None;
        }
        loop {
            let block = self.immix.reusable_blocks.pop()?;
            // Skip blocks that should be evacuated.
            if copy && block.is_defrag_source() {
                continue;
            }
            if LXR_MATURE_EVACUATION && block.is_defrag_source() {
                continue;
            }
            // Blocks in the `reusable_blocks` queue can be released after some RC collections.
            // These blocks can either have `Unallocated` state, or be reallocated again.
            // Skip these cases and only return the truly reusable blocks.
            if !block.get_state().is_reusable() {
                continue;
            }
            if !block.attempt_mutator_reuse() {
                continue;
            }
            if let Some(hooks) = self.hooks() {
                hooks.on_reusable_block_acquired(block, copy);
            }
            block.init(copy, true, true);
            return Some(block);
        }
    }

    fn get_next_available_lines(&self, copy: bool, search_start: Line) -> Option<(Line, Line)> {
        self.rc_get_next_available_lines(copy, search_start)
    }

    fn post_copy(&self, _object: ObjectReference, _bytes: usize) {
        // LXR's mature-evacuation path (`trace_forward_rc_mature_object`) sets marks and
        // transfers the RC count itself; the generic copy-context post-copy hook is a no-op.
    }

    fn in_defrag(&self) -> bool {
        self.immix.in_defrag()
    }
}

impl<VM: VMBinding> LXRSpace<VM> {
    pub fn new(args: PlanCreateSpaceArgs<VM>, space_args: ImmixSpaceArgs) -> Self {
        use crate::util::metadata::side_metadata::spec_defs::IX_LINE_REUSE_COUNT;
        use crate::util::metadata::MetadataSpec;
        LXRSpace {
            immix: ImmixSpace::new_with_extra_side_metadata_specs(
                args,
                space_args,
                vec![
                    MetadataSpec::OnSide(crate::util::rc::RC_STRADDLE_LINES),
                    MetadataSpec::OnSide(Block::LOG_TABLE),
                    MetadataSpec::OnSide(Block::NURSERY_PROMOTION_STATE_TABLE),
                    MetadataSpec::OnSide(IX_LINE_REUSE_COUNT),
                ],
            ),
            hooks: OnceLock::new(),
            is_end_of_satb_or_full_gc: false,
            rc: RefCountHelper::NEW,
        }
    }

    /// Install the plan-level hooks. Called once by LXR during `gc_init`.
    pub fn install_hooks(&self, hooks: &'static dyn ImmixHooks<VM>) {
        self.hooks
            .set(hooks)
            .unwrap_or_else(|_| panic!("LXRSpace::install_hooks called more than once"));
    }

    fn hooks(&self) -> Option<&'static dyn ImmixHooks<VM>> {
        self.hooks.get().copied()
    }

    /// Access the inner, generic `ImmixSpace`. Used by `Block`'s RC-only sweeping methods, which
    /// only need the shared block/chunk/page-resource machinery and have no RC-specific
    /// dependency of their own.
    pub(crate) fn inner(&self) -> &ImmixSpace<VM> {
        &self.immix
    }

    pub fn scheduler(&self) -> &GCWorkScheduler<VM> {
        self.immix.scheduler()
    }

    pub fn defrag_headroom_pages(&self) -> usize {
        self.immix.defrag_headroom_pages()
    }

    pub fn chunk_map(&self) -> &crate::util::heap::chunk_map::ChunkMap {
        &self.immix.chunk_map
    }

    pub fn attempt_mark(&self, object: ObjectReference) -> bool {
        self.immix.attempt_mark(object)
    }

    pub fn unmark(&self, object: ObjectReference) -> bool {
        self.immix.unmark(object)
    }

    pub(crate) fn is_marked(&self, object: ObjectReference) -> bool {
        self.immix.is_marked(object)
    }

    pub fn flush_page_resource(&self) {
        // FIXME: Do we need this for LXR? We observed this to cause fails on conix.
        self.immix.flush_pr_only();
    }

    pub fn prepare_rc(&mut self, pause: Pause) {
        // Initialize mark state for tracing
        if pause == Pause::Full || pause == Pause::InitialMark {
            self.immix.reset_mark_state_to_marked();
        }
        // Release nursery blocks
        if pause != Pause::RefCount {
            if pause == Pause::Full {
                // Reset worker TLABs.
                // The block of the current worker TLAB may be selected as part of the mature evacuation set.
                for w in &self.scheduler().worker_group.workers_shared {
                    let result = w.designated_work.push(Box::new(PrepareCollector));
                    debug_assert!(result.is_ok());
                }
            }
            self.flush_page_resource();
        }
        if pause == Pause::FinalMark || pause == Pause::Full {
            self.is_end_of_satb_or_full_gc = true;
        }
    }

    pub fn release_rc(&mut self) {
        self.flush_page_resource();
        self.rc.reset_inc_buffer_size();
        self.is_end_of_satb_or_full_gc = false;
        self.immix.reset_reused_lines_consumed();
    }

    pub fn trace_object_without_moving_rc(
        &self,
        queue: &mut impl ObjectQueue,
        object: ObjectReference,
    ) -> ObjectReference {
        if self.attempt_mark(object) {
            let addr = object.to_raw_address().as_usize();
            let straddle = if (addr & 0b11110000) == 0 {
                self.rc.is_straddle_line(Line::containing_obj_ref(object))
            } else {
                false
            };
            if !straddle {
                queue.enqueue(object);
            }
        }
        object
    }

    pub fn rc_trace_object<Q: ObjectQueue>(
        &self,
        queue: &mut Q,
        object: ObjectReference,
        semantics: CopySemantics,
        pause: Pause,
        mark: bool,
        worker: &mut GCWorker<VM>,
    ) -> ObjectReference {
        if LXR_MATURE_EVACUATION && Block::containing(object).is_defrag_source() {
            self.trace_forward_rc_mature_object(queue, object, semantics, pause, worker)
        } else if LXR_MATURE_EVACUATION {
            self.trace_mark_rc_mature_object(queue, object, pause, mark)
        } else {
            self.trace_object_without_moving_rc(queue, object)
        }
    }

    pub fn trace_mark_rc_mature_object(
        &self,
        queue: &mut impl ObjectQueue,
        object: ObjectReference,
        _pause: Pause,
        mark: bool,
    ) -> ObjectReference {
        debug_assert!(
            !object_forwarding::is_forwarded::<VM>(object),
            "object {:?} is forwarded",
            object
        );
        if mark && self.attempt_mark(object) {
            queue.enqueue(object);
        }
        object
    }

    #[allow(clippy::assertions_on_constants)]
    pub fn trace_forward_rc_mature_object<Q: ObjectQueue>(
        &self,
        queue: &mut Q,
        object: ObjectReference,
        _semantics: CopySemantics,
        _pause: Pause,
        worker: &mut GCWorker<VM>,
    ) -> ObjectReference {
        let copy_context = worker.get_copy_context_mut();
        let forwarding_status = object_forwarding::attempt_to_forward::<VM>(object);
        if object_forwarding::state_is_forwarded_or_being_forwarded(forwarding_status) {
            object_forwarding::spin_and_get_forwarded_object::<VM>(object, forwarding_status)
        } else {
            // Evacuate the mature object
            let new = object_forwarding::try_forward_object::<VM>(
                object,
                CopySemantics::DefaultCopy,
                copy_context,
                |_new_object| {
                    // When using RC, we set the VO bit of the forwarded object.
                    #[cfg(feature = "vo_bit")]
                    crate::util::metadata::vo_bit::set_vo_bit(_new_object);
                },
            )
            .expect("to-space overflow");
            // Transfer RC count
            if new.get_size::<VM>() > Line::BYTES {
                self.rc.mark_straddle_object(new);
            }
            self.rc.set(new, self.rc.count(object));
            self.attempt_mark(new);
            self.unmark(object);
            queue.enqueue(new);
            debug_assert_ne!(
                self.rc.count(new),
                0,
                "ERROR Invalid {:?} rc={}",
                new,
                self.rc.count(new)
            );
            new
        }
    }

    /// Search holes by ref-counts instead of line marks
    #[allow(clippy::assertions_on_constants)]
    pub fn rc_get_next_available_lines(
        &self,
        copy: bool,
        search_start: Line,
    ) -> Option<(Line, Line)> {
        debug_assert!(!crate::policy::immix::BLOCK_ONLY);
        let block = search_start.block();
        let rc_array = RCArray::of(block);
        let limit = Block::LINES;
        // Find start
        let first_free_cursor = {
            let start_cursor = search_start.get_index_within_block();
            let mut first_free_cursor = None;
            let mut find_free_line = false;
            for i in start_cursor..limit {
                if rc_array.is_dead(i) {
                    if i == 0 {
                        first_free_cursor = Some(i);
                        break;
                    } else if !find_free_line {
                        // This skips the first line of a hole
                        // because `mark_straddle_object_with_size` may or may not set the RC
                        // of the last line an object straddles.
                        find_free_line = true;
                    } else {
                        first_free_cursor = Some(i);
                        break;
                    }
                } else {
                    find_free_line = false;
                }
            }
            first_free_cursor
        };
        let start = match first_free_cursor {
            Some(c) => c,
            _ => return None,
        };
        // Find limit
        let end = {
            let mut cursor = start + 1;
            while cursor < limit {
                if !rc_array.is_dead(cursor) {
                    break;
                }
                cursor += 1;
            }
            cursor
        };
        let start = Line::from_aligned_address(block.start()).next_nth(start);
        let end = Line::from_aligned_address(block.start()).next_nth(end);
        if self.immix.common().needs_log_bit {
            if !copy {
                Line::clear_field_unlog_table::<VM>(start..end);
            } else {
                Line::initialize_field_unlog_table_as_unlogged::<VM>(start..end);
            }
        }
        let num_lines = Line::steps_between(&start, &end).unwrap();
        if !copy {
            self.immix.add_reused_lines_consumed(num_lines);
        }
        if self
            .hooks()
            .is_some_and(|h| h.cm_in_progress_or_final_mark())
        {
            Line::initialize_mark_table_as_marked::<VM>(start..end);
            Line::inc_reuse_counts(start..end);
        }
        Some((start, end))
    }

    pub(crate) fn get_mutator_recycled_lines_in_pages(&self) -> usize {
        self.immix.reused_lines_consumed_count()
            >> (crate::util::constants::LOG_BYTES_IN_PAGE - Line::LOG_BYTES as u8)
    }
}
