<<<<<<< HEAD
use crate::policy::sft::SFT;
use crate::policy::space::{CommonSpace, Space};
use crate::vm::{ObjectModel, VMBinding};
use crate::policy::immortalspace::ImmortalSpace;
use crate::util::{metadata, ObjectReference};
use crate::plan::{ObjectQueue, VectorObjectQueue};
use crate::policy::sft::GCWorkerMutRef;
use crate::util::heap::{MonotonePageResource, PageResource};
use crate::util::address::Address;
use crate::plan::{CreateGeneralPlanArgs, CreateSpecificPlanArgs};
use crate::util::heap::VMRequest;
use crate::util::metadata::side_metadata::SideMetadataContext;
use crate::util::metadata::side_metadata::SideMetadataSanity;
use crate::util::heap::HeapMeta;
=======
use crate::plan::{CreateGeneralPlanArgs, CreateSpecificPlanArgs};
use crate::plan::{ObjectQueue, VectorObjectQueue};
use crate::policy::immortalspace::ImmortalSpace;
use crate::policy::sft::GCWorkerMutRef;
use crate::policy::sft::SFT;
use crate::policy::space::{CommonSpace, Space};
use crate::util::address::Address;
use crate::util::heap::HeapMeta;
use crate::util::heap::PageResource;
use crate::util::heap::VMRequest;
use crate::util::metadata::side_metadata::SideMetadataContext;
use crate::util::metadata::side_metadata::SideMetadataSanity;
use crate::util::ObjectReference;
use crate::vm::VMBinding;

use delegate::delegate;
>>>>>>> master

pub struct VMSpace<VM: VMBinding> {
    inner: Option<ImmortalSpace<VM>>,
    // Save it
    args: CreateSpecificPlanArgs<VM>,
}

impl<VM: VMBinding> SFT for VMSpace<VM> {
<<<<<<< HEAD
    fn name(&self) -> &str {
        self.space().name()
    }
    fn is_live(&self, object: ObjectReference) -> bool {
        self.space().is_live(object)
    }
    fn is_reachable(&self, object: ObjectReference) -> bool {
        self.space().is_reachable(object)
    }
    #[cfg(feature = "object_pinning")]
    fn pin_object(&self, object: ObjectReference) -> bool {
        self.space().pin_object(object)
    }
    #[cfg(feature = "object_pinning")]
    fn unpin_object(&self, object: ObjectReference) -> bool {
        self.space().unpin_object(object)
    }
    #[cfg(feature = "object_pinning")]
    fn is_object_pinned(&self, _object: ObjectReference) -> bool {
        self.space().is_object_pinned(object)
    }
    fn is_movable(&self) -> bool {
        self.space().is_movable()
    }
    #[cfg(feature = "sanity")]
    fn is_sane(&self) -> bool {
        self.space().is_sane()
    }
    fn initialize_object_metadata(&self, object: ObjectReference, _alloc: bool) {
        unreachable!()
    }
    #[cfg(feature = "is_mmtk_object")]
    fn is_mmtk_object(&self, addr: Address) -> bool {
        self.space().is_mmtk_object(addr)
    }
    fn sft_trace_object(
        &self,
        queue: &mut VectorObjectQueue,
        object: ObjectReference,
        worker: GCWorkerMutRef,
    ) -> ObjectReference {
        self.space().sft_trace_object(queue, object, worker)
=======
    delegate! {
        // Delegate every call to the inner space. Given that we have acquired SFT, we can assume there are objects in the space and the space is initialized.
        to self.space() {
            fn name(&self) -> &str;
            fn is_live(&self, object: ObjectReference) -> bool;
            fn is_reachable(&self, object: ObjectReference) -> bool;
            #[cfg(feature = "object_pinning")]
            fn pin_object(&self, object: ObjectReference) -> bool;
            #[cfg(feature = "object_pinning")]
            fn unpin_object(&self, object: ObjectReference) -> bool;
            #[cfg(feature = "object_pinning")]
            fn is_object_pinned(&self, object: ObjectReference) -> bool;
            fn is_movable(&self) -> bool;
            #[cfg(feature = "sanity")]
            fn is_sane(&self) -> bool;
            fn initialize_object_metadata(&self, object: ObjectReference, alloc: bool);
            #[cfg(feature = "is_mmtk_object")]
            fn is_mmtk_object(&self, addr: Address) -> bool;
            fn sft_trace_object(
                &self,
                queue: &mut VectorObjectQueue,
                object: ObjectReference,
                worker: GCWorkerMutRef,
            ) -> ObjectReference;
        }
>>>>>>> master
    }
}

impl<VM: VMBinding> Space<VM> for VMSpace<VM> {
    fn as_space(&self) -> &dyn Space<VM> {
        self
    }
    fn as_sft(&self) -> &(dyn SFT + Sync + 'static) {
        self
    }
    fn get_page_resource(&self) -> &dyn PageResource<VM> {
        self.space().get_page_resource()
    }
    fn common(&self) -> &CommonSpace<VM> {
        self.space().common()
    }

    fn initialize_sft(&self) {
        if self.inner.is_some() {
            self.common().initialize_sft(self.as_sft())
        }
    }

    fn release_multiple_pages(&mut self, _start: Address) {
        panic!("immortalspace only releases pages enmasse")
    }

    fn verify_side_metadata_sanity(&self, side_metadata_sanity_checker: &mut SideMetadataSanity) {
<<<<<<< HEAD
        side_metadata_sanity_checker
            .verify_metadata_context(std::any::type_name::<Self>(), &SideMetadataContext {
                global: self.args.global_side_metadata_specs.clone(),
                local: vec![],
            })
=======
        side_metadata_sanity_checker.verify_metadata_context(
            std::any::type_name::<Self>(),
            &SideMetadataContext {
                global: self.args.global_side_metadata_specs.clone(),
                local: vec![],
            },
        )
>>>>>>> master
    }

    fn address_in_space(&self, start: Address) -> bool {
        if let Some(space) = self.space_maybe() {
            space.address_in_space(start)
        } else {
            false
        }
    }
}

use crate::scheduler::GCWorker;
use crate::util::copy::CopySemantics;

impl<VM: VMBinding> crate::policy::gc_work::PolicyTraceObject<VM> for VMSpace<VM> {
    fn trace_object<Q: ObjectQueue, const KIND: crate::policy::gc_work::TraceKind>(
        &self,
        queue: &mut Q,
        object: ObjectReference,
        _copy: Option<CopySemantics>,
        _worker: &mut GCWorker<VM>,
    ) -> ObjectReference {
        self.trace_object(queue, object)
    }
    fn may_move_objects<const KIND: crate::policy::gc_work::TraceKind>() -> bool {
        false
    }
}

impl<VM: VMBinding> VMSpace<VM> {
    pub fn new(args: &mut CreateSpecificPlanArgs<VM>) -> Self {
        let args_clone = CreateSpecificPlanArgs {
            global_args: CreateGeneralPlanArgs {
                vm_map: args.global_args.vm_map,
                mmapper: args.global_args.mmapper,
                heap: HeapMeta::new(), // we do not use this
                options: args.global_args.options.clone(),
                gc_trigger: args.global_args.gc_trigger.clone(),
                scheduler: args.global_args.scheduler.clone(),
            },
            constraints: args.constraints,
            global_side_metadata_specs: args.global_side_metadata_specs.clone(),
        };
<<<<<<< HEAD
        if !args.global_args.options.vm_space_start.is_zero() {
            Self { inner: Some(Self::create_space(args, None)), args: args_clone }
        } else {
            Self { inner: None, args: args_clone }
=======
        // Create the space if the VM space start/size is set. Otherwise, use None.
        let inner = (!args.global_args.options.vm_space_start.is_zero())
            .then(|| Self::create_space(args, None));
        Self {
            inner,
            args: args_clone,
>>>>>>> master
        }
    }

    pub fn lazy_initialize(&mut self, start: Address, size: usize) {
        assert!(self.inner.is_none(), "VM space has been initialized");
        self.inner = Some(Self::create_space(&mut self.args, Some((start, size))));

        self.common().initialize_sft(self.as_sft());
    }

<<<<<<< HEAD
    fn create_space(args: &mut CreateSpecificPlanArgs<VM>, location: Option<(Address, usize)>) -> ImmortalSpace<VM> {
        use crate::util::constants::LOG_BYTES_IN_MBYTE;
        use crate::util::conversions::raw_align_up;
        use crate::util::heap::layout::vm_layout_constants::BYTES_IN_CHUNK;

        let (boot_segment_start, boot_segment_bytes) = if let Some((start, size)) = location {
            (start, size)
        } else {
            (*args.global_args.options.vm_space_start, *args.global_args.options.vm_space_size)
        };

        assert!(!boot_segment_start.is_zero());
        assert!(boot_segment_bytes > 0);

        // #[cfg(target_pointer_width = "32")]
        // let (boot_segment_start_aligned, boot_segment_bytes_aligned) = (boot_segment_start.align_down(BYTES_IN_CHUNK), raw_align_up(boot_segment_bytes, BYTES_IN_CHUNK));
        // #[cfg(target_pointer_width = "64")]
        // let (boot_segment_start_aligned, boot_segment_bytes_aligned) = {
        //     let space_index = |addr: Address| {
        //         addr >> crate::util::heap::layout::vm_layout_constants::SPACE_SHIFT_64
        //     };
        //     let end = boot_segment_start + boot_segment_bytes;
        //     let space_start = unsafe { Address::from_usize(boot_segment_start & !((1 << crate::util::heap::layout::vm_layout_constants::SPACE_SHIFT_64) - 1usize)) };
        //     let size_to_end = raw_align_up(end - space_start, BYTES_IN_CHUNK);

        //     info!("Index for unaligned {} is {}, Index for aligned {} is {}", boot_segment_start, space_index(boot_segment_start), space_start, space_index(space_start));
        //     (space_start, size_to_end)
        // };
        let (boot_segment_start_aligned, boot_segment_bytes_aligned) = (boot_segment_start.align_down(BYTES_IN_CHUNK), raw_align_up(boot_segment_bytes, BYTES_IN_CHUNK));


        // let space = ImmortalSpace::new(args.get_space_args(
        //     "boot",
        //     false,
        //     VMRequest::fixed_size(boot_segment_mb),
        // ));
        info!("start {} is aligned to {}, bytes = {}", boot_segment_start, boot_segment_start_aligned, boot_segment_bytes_aligned);
        // As we create an immortal space, we use the same side metadata as immortal space.
        let side_metadata = metadata::extract_side_metadata(&[*VM::VMObjectModel::LOCAL_MARK_BIT_SPEC]);
        let space = ImmortalSpace::new_customized(
            MonotonePageResource::new_contiguous(boot_segment_start_aligned, boot_segment_bytes_aligned, args.global_args.vm_map),
            CommonSpace::new(args.get_space_args("vm_spce", false, VMRequest::fixed(boot_segment_start_aligned, boot_segment_bytes_aligned)).into_vm_space_args(side_metadata)),
        );

=======
    fn create_space(
        args: &mut CreateSpecificPlanArgs<VM>,
        location: Option<(Address, usize)>,
    ) -> ImmortalSpace<VM> {
        use crate::util::heap::layout::vm_layout_constants::BYTES_IN_CHUNK;

        // If the location of the VM space is not supplied, find them in the options.
        let (vm_space_start, vm_space_bytes) = location.unwrap_or((
            *args.global_args.options.vm_space_start,
            *args.global_args.options.vm_space_size,
        ));
        // Verify the start and the size is valid
        assert!(!vm_space_start.is_zero());
        assert!(vm_space_bytes > 0);

        // We only map on chunk granularity. Align them.
        let vm_space_start_aligned = vm_space_start.align_down(BYTES_IN_CHUNK);
        let vm_space_end_aligned = (vm_space_start + vm_space_bytes).align_up(BYTES_IN_CHUNK);
        let vm_space_bytes_aligned = vm_space_end_aligned - vm_space_start_aligned;

        // For simplicity, VMSpace has to be outside our available heap range.
        // TODO: Allow VMSpace in our available heap range.
        assert!(Address::range_intersection(
            &(vm_space_start_aligned..vm_space_end_aligned),
            &crate::util::heap::layout::available_range()
        )
        .is_empty());

        debug!(
            "Align VM space ({}, {}) to chunk ({}, {})",
            vm_space_start,
            vm_space_start + vm_space_bytes,
            vm_space_start_aligned,
            vm_space_end_aligned
        );

        let space_args = args.get_space_args(
            "vm_space",
            false,
            VMRequest::fixed(vm_space_start_aligned, vm_space_bytes_aligned),
        );
        let space =
            ImmortalSpace::new_vm_space(space_args, vm_space_start_aligned, vm_space_bytes_aligned);

>>>>>>> master
        // The space is mapped externally by the VM. We need to update our mmapper to mark the range as mapped.
        space.ensure_mapped();

        space
    }

<<<<<<< HEAD
=======
    fn space_maybe(&self) -> Option<&ImmortalSpace<VM>> {
        self.inner.as_ref()
    }

    fn space(&self) -> &ImmortalSpace<VM> {
        self.inner.as_ref().unwrap()
    }

    // fn space_mut(&mut self) -> &mut ImmortalSpace<VM> {
    //     self.inner.as_mut().unwrap()
    // }

>>>>>>> master
    pub fn prepare(&mut self) {
        if let Some(ref mut space) = &mut self.inner {
            space.prepare()
        }
    }

    pub fn release(&mut self) {
        if let Some(ref mut space) = &mut self.inner {
            space.release()
        }
    }

    pub fn trace_object<Q: ObjectQueue>(
        &self,
        queue: &mut Q,
        object: ObjectReference,
    ) -> ObjectReference {
        if let Some(ref space) = &self.inner {
            space.trace_object(queue, object)
        } else {
            panic!("We haven't initialized vm space, but we tried to trace the object {} and thought it was in vm space?", object)
        }
    }
<<<<<<< HEAD

    fn space_maybe(&self) -> Option<&ImmortalSpace<VM>> {
        self.inner.as_ref()
    }

    fn space(&self) -> &ImmortalSpace<VM> {
        self.inner.as_ref().unwrap()
    }

    fn space_mut(&mut self) -> &mut ImmortalSpace<VM> {
        self.inner.as_mut().unwrap()
    }
}
=======
}
>>>>>>> master
