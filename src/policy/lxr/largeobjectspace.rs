//! LXR's ref-counting flavor of [`LargeObjectSpace`].

use atomic::Ordering;

use crate::plan::tracing::{ObjectQueue, OptionObjectQueue};
use crate::policy::gc_work::{PolicyTraceObject, TraceKind};
use crate::policy::largeobjectspace::{self, LargeObjectSpace, LOS_BIT_MASK, MARK_BIT};
use crate::policy::sft::GCWorkerMutRef;
use crate::policy::sft::SFT;
use crate::policy::space::{CommonSpace, PlanCreateSpaceArgs, Space};
use crate::scheduler::GCWorker;
use crate::util::constants::BYTES_IN_PAGE;
use crate::util::copy::CopySemantics;
use crate::util::heap::PageResource;
use crate::util::metadata::side_metadata::spec_defs::LOS_PAGE_REUSE_COUNT;
use crate::util::object_enum::ObjectEnumerator;
use crate::util::opaque_pointer::*;
use crate::util::rc::RefCountHelper;
use crate::util::{Address, ObjectReference};
use crate::vm::ObjectModel;
use crate::vm::VMBinding;
use std::sync::atomic::AtomicBool;

/// LXR's ref-counting flavor of [`LargeObjectSpace`]. Wraps a generic, tracing-only
/// `LargeObjectSpace` and layers reference-counting semantics on top (RC-based liveness, RC-only
/// nursery/mature sweeping), reusing the inner space's treadmill/page-resource machinery.
pub struct LXRLargeObjectSpace<VM: VMBinding> {
    los: LargeObjectSpace<VM>,
    pub rc: RefCountHelper<VM>,
    pub is_end_of_satb_or_full_gc: bool,
    /// Whether newly allocated LOS objects should bump `LOS_PAGE_REUSE_COUNT` on their pages, so
    /// remembered-set entries recorded against a page's previous occupant are invalidated. Only
    /// needed while concurrent marking can be validating a remembered set; set/cleared by LXR as
    /// concurrent marking starts/ends.
    pub(crate) bump_page_reuse_count: AtomicBool,
}

impl<VM: VMBinding> SFT for LXRLargeObjectSpace<VM> {
    fn name(&self) -> &'static str {
        self.get_name()
    }
    fn is_live(&self, object: ObjectReference) -> bool {
        if self.is_end_of_satb_or_full_gc {
            return self.is_marked(object) && self.rc.count(object) > 0;
        }
        self.rc.count(object) > 0
    }
    fn is_reachable(&self, object: ObjectReference) -> bool {
        self.is_marked(object) && self.rc.count(object) > 0
    }
    #[cfg(feature = "object_pinning")]
    fn pin_object(&self, _object: ObjectReference) -> bool {
        false
    }
    #[cfg(feature = "object_pinning")]
    fn unpin_object(&self, _object: ObjectReference) -> bool {
        false
    }
    #[cfg(feature = "object_pinning")]
    fn is_object_pinned(&self, _object: ObjectReference) -> bool {
        true
    }
    fn is_movable(&self) -> bool {
        false
    }
    #[cfg(feature = "sanity")]
    fn is_sane(&self) -> bool {
        true
    }

    fn initialize_object_metadata(&self, object: ObjectReference, bytes: usize) {
        // VO bit: Set for all objects.
        #[cfg(feature = "vo_bit")]
        crate::util::metadata::vo_bit::set_vo_bit(object);
        #[cfg(all(feature = "vo_bit", debug_assertions))]
        {
            use crate::util::constants::LOG_BYTES_IN_PAGE;
            let vo_addr = object.to_raw_address();
            let offset_from_page_start = vo_addr & ((1 << LOG_BYTES_IN_PAGE) - 1) as usize;
            debug_assert!(
                offset_from_page_start < crate::util::metadata::vo_bit::VO_BIT_WORD_TO_REGION,
                "The raw address of ObjectReference is not in the first 512 bytes of a page. The internal pointer searching for LOS won't work."
            );
        }

        // Add to treadmill nursery
        self.los.treadmill_add(object, true);
        // Initialize mark bit
        self.test_and_mark(object);
        // Initialize metadata
        if self.bump_page_reuse_count.load(Ordering::Acquire) {
            for off in (0..bytes).step_by(BYTES_IN_PAGE) {
                let a = object.to_raw_address() + off;
                let count = LOS_PAGE_REUSE_COUNT.load_atomic::<u8>(a, Ordering::SeqCst);
                let new_count = if count == u8::MAX { 0 } else { count + 1 };
                LOS_PAGE_REUSE_COUNT.store_atomic::<u8>(a, new_count, Ordering::SeqCst);
            }
        }
    }

    #[cfg(feature = "vo_bit")]
    fn is_mmtk_object(&self, addr: Address) -> Option<ObjectReference> {
        self.los.is_mmtk_object(addr)
    }
    #[cfg(feature = "vo_bit")]
    fn find_object_from_internal_pointer(
        &self,
        ptr: Address,
        max_search_bytes: usize,
    ) -> Option<ObjectReference> {
        self.los.find_object_from_internal_pointer(ptr, max_search_bytes)
    }
    fn sft_trace_object(
        &self,
        queue: &mut OptionObjectQueue,
        object: ObjectReference,
        _worker: GCWorkerMutRef,
    ) -> ObjectReference {
        self.trace_object(queue, object)
    }

    fn debug_print_object_info(&self, object: ObjectReference) {
        println!("marked = {}", self.is_marked(object));
        self.los.common().debug_print_object_global_info(object);
    }
}

impl<VM: VMBinding> Space<VM> for LXRLargeObjectSpace<VM> {
    fn as_space(&self) -> &dyn Space<VM> {
        self
    }
    fn as_sft(&self) -> &(dyn SFT + Sync + 'static) {
        self
    }
    fn get_page_resource(&self) -> &dyn PageResource<VM> {
        self.los.get_page_resource()
    }
    fn maybe_get_page_resource_mut(&mut self) -> Option<&mut dyn PageResource<VM>> {
        self.los.maybe_get_page_resource_mut()
    }
    fn initialize_sft(&self, sft_map: &mut dyn crate::policy::sft_map::SFTMap) {
        // Register *this* wrapper (not the inner `LargeObjectSpace`) as the SFT for this space,
        // so liveness/tracing dispatch goes through the RC-flavored impls above.
        self.common().initialize_sft(self.as_sft(), sft_map)
    }
    fn common(&self) -> &CommonSpace<VM> {
        self.los.common()
    }
    fn release_multiple_pages(&mut self, start: Address) {
        self.los.release_multiple_pages(start)
    }
    fn enumerate_objects(&self, enumerator: &mut dyn ObjectEnumerator) {
        self.los.enumerate_objects(enumerator)
    }
    fn clear_side_log_bits(&self) {
        self.los.clear_side_log_bits()
    }
    fn set_side_log_bits(&self) {
        self.los.set_side_log_bits()
    }
}

impl<VM: VMBinding> PolicyTraceObject<VM> for LXRLargeObjectSpace<VM> {
    fn trace_object<Q: ObjectQueue, const KIND: TraceKind>(
        &self,
        queue: &mut Q,
        object: ObjectReference,
        _copy: Option<CopySemantics>,
        _worker: &mut GCWorker<VM>,
    ) -> ObjectReference {
        self.trace_object(queue, object)
    }
    fn may_move_objects<const KIND: TraceKind>() -> bool {
        false
    }
}

impl<VM: VMBinding> crate::policy::largeobjectspace::LargeObjectSpaceExt<VM> for LXRLargeObjectSpace<VM> {
    fn allocate_pages(
        &self,
        tls: VMThread,
        pages: usize,
        alloc_options: crate::util::alloc::allocator::AllocationOptions,
    ) -> Address {
        self.los.allocate_pages(tls, pages, alloc_options)
    }
}

impl<VM: VMBinding> LXRLargeObjectSpace<VM> {
    pub fn new(
        args: PlanCreateSpaceArgs<VM>,
        protect_memory_on_release: bool,
        clear_log_bit_on_sweep: bool,
    ) -> Self {
        LXRLargeObjectSpace {
            los: LargeObjectSpace::new_with_extra_side_metadata_specs(
                args,
                protect_memory_on_release,
                clear_log_bit_on_sweep,
                vec![crate::util::metadata::MetadataSpec::OnSide(
                    LOS_PAGE_REUSE_COUNT,
                )],
            ),
            rc: RefCountHelper::NEW,
            is_end_of_satb_or_full_gc: false,
            bump_page_reuse_count: AtomicBool::new(false),
        }
    }

    pub fn attempt_mark(&self, object: ObjectReference) -> bool {
        self.test_and_mark(object)
    }

    pub fn num_pages_released_lazy(&self) -> usize {
        self.los.num_pages_released_lazy.load(Ordering::SeqCst)
    }

    pub fn is_marked(&self, object: ObjectReference) -> bool {
        self.test_mark_bit(object)
    }

    /// Test if the object's mark bit is set to the current mark state. If not, attempt to mark
    /// it. Returns `true` if this call marked the object. Unlike the generic tracing
    /// `LargeObjectSpace::test_and_mark`, RC never distinguishes a nursery-GC mask: the mark bit
    /// alone determines liveness.
    fn test_and_mark(&self, object: ObjectReference) -> bool {
        let value = self.los.mark_state();
        loop {
            let old_value = VM::VMObjectModel::LOCAL_LOS_MARK_NURSERY_SPEC.load_atomic::<VM, u8>(
                object,
                None,
                Ordering::SeqCst,
            );
            let mark_bit = old_value & MARK_BIT;
            if mark_bit == value {
                return false;
            }
            if VM::VMObjectModel::LOCAL_LOS_MARK_NURSERY_SPEC
                .compare_exchange_metadata::<VM, u8>(
                    object,
                    old_value,
                    old_value & !LOS_BIT_MASK | value,
                    None,
                    Ordering::SeqCst,
                    Ordering::SeqCst,
                )
                .is_ok()
            {
                break;
            }
        }
        true
    }

    fn test_mark_bit(&self, object: ObjectReference) -> bool {
        VM::VMObjectModel::LOCAL_LOS_MARK_NURSERY_SPEC.load_atomic::<VM, u8>(
            object,
            None,
            Ordering::SeqCst,
        ) & MARK_BIT
            == self.los.mark_state()
    }

    fn release_object(&self, object: ObjectReference) -> usize {
        #[cfg(feature = "vo_bit")]
        crate::util::metadata::vo_bit::unset_vo_bit(object);
        debug_assert_eq!(self.rc.count(object), 0);
        let start = largeobjectspace::get_super_page(object.to_object_start::<VM>());
        let pages = self.los.pages_for_start(start);
        // TODO: Currently this code path assumes the collector is LXR and it uses field log bit.
        // When we can use object log bit for LXR, we should merge with the tracing sweep path
        // and clear object log bit instead.
        VM::VMObjectModel::GLOBAL_FIELD_UNLOG_BIT_SPEC
            .as_spec()
            .extract_side_spec()
            .bzero_metadata(start, pages * BYTES_IN_PAGE);
        self.los.release_pages_at(start)
    }

    pub fn release_rc_nursery_objects(&self) {
        // promote nursery objects or release dead nursery
        for o in self.los.treadmill_collect_alloc_nursery() {
            if self.rc.count(o) == 0 {
                self.release_object(o);
            } else {
                self.los.treadmill_add(o, false);
            }
        }
    }

    pub fn prepare(&mut self, _full_heap: bool) {
        if _full_heap {
            self.los.flip_mark_state();
        }
        self.los.num_pages_released_lazy.store(0, Ordering::Relaxed);
    }

    pub fn release(&mut self, _full_heap: bool) {
        self.release_rc_nursery_objects();
    }

    pub fn trace_object<Q: ObjectQueue>(
        &self,
        queue: &mut Q,
        object: ObjectReference,
    ) -> ObjectReference {
        #[cfg(feature = "vo_bit")]
        debug_assert!(
            crate::util::metadata::vo_bit::is_vo_bit_set(object),
            "{:x}: VO bit not set",
            object
        );
        if self.test_and_mark(object) {
            queue.enqueue(object);
        }
        object
    }

    pub fn rc_free(&self, o: ObjectReference) {
        if self.los.treadmill_remove_mature(o) {
            let pages = self.release_object(o);
            self.los
                .num_pages_released_lazy
                .fetch_add(pages, Ordering::Relaxed);
        }
    }

    pub fn sweep_rc_mature_objects_after_satb(&self, is_live: &impl Fn(ObjectReference) -> bool) {
        self.los.treadmill_retain_mature(|o| {
            if !is_live(*o) {
                self.rc.set(*o, 0);
                let pages = self.release_object(*o);
                self.los
                    .num_pages_released_lazy
                    .fetch_add(pages, Ordering::Relaxed);
                false
            } else {
                true
            }
        });
    }
}
