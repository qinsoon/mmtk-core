use super::barrier::LXRFieldBarrierSemantics;
use super::LXR;
use crate::plan::barriers::FieldBarrier;
use crate::plan::mutator_context::create_allocator_mapping;
use crate::plan::mutator_context::create_space_mapping;
use crate::plan::mutator_context::Mutator;
use crate::plan::mutator_context::MutatorConfig;
use crate::plan::mutator_context::ReservedAllocators;
use crate::plan::AllocationSemantics;
use crate::util::alloc::allocators::{AllocatorSelector, Allocators};
use crate::util::alloc::ImmixAllocator;
use crate::util::opaque_pointer::{VMMutatorThread, VMWorkerThread};
use crate::vm::VMBinding;
use crate::MMTK;
use enum_map::EnumMap;

pub fn lxr_mutator_prepare<VM: VMBinding>(mutator: &mut Mutator<VM>, _tls: VMWorkerThread) {
    let immix_allocator = unsafe {
        mutator
            .allocators
            .get_allocator_mut(mutator.config.allocator_mapping[AllocationSemantics::Default])
    }
    .downcast_mut::<ImmixAllocator<VM>>()
    .unwrap();
    immix_allocator.reset();
}

pub fn lxr_mutator_release<VM: VMBinding>(mutator: &mut Mutator<VM>, _tls: VMWorkerThread) {
    let immix_allocator = unsafe {
        mutator
            .allocators
            .get_allocator_mut(mutator.config.allocator_mapping[AllocationSemantics::Default])
    }
    .downcast_mut::<ImmixAllocator<VM>>()
    .unwrap();
    immix_allocator.reset();
}

// LXR does not use `CommonPlan` (Immortal/NonMoving allocation never worked properly for it), so
// we don't ask `create_allocator_mapping`/`create_space_mapping` to wire those up. LOS is LXR's
// own field (a ref-counting flavored space, not `CommonPlan`'s), so we reserve and wire it up
// manually here, the same way immix_space is.
const RESERVED_ALLOCATORS: ReservedAllocators = ReservedAllocators {
    n_immix: 1,
    n_large_object: 1,
    ..ReservedAllocators::DEFAULT
};

lazy_static! {
    pub static ref ALLOCATOR_MAPPING: EnumMap<AllocationSemantics, AllocatorSelector> = {
        let mut map = create_allocator_mapping(RESERVED_ALLOCATORS, false);
        map[AllocationSemantics::Default] = AllocatorSelector::Immix(0);
        map[AllocationSemantics::Los] = AllocatorSelector::LargeObject(0);
        map
    };
}

pub fn create_lxr_mutator<VM: VMBinding>(
    mutator_tls: VMMutatorThread,
    mmtk: &'static MMTK<VM>,
) -> Mutator<VM> {
    let lxr = mmtk.get_plan().downcast_ref::<LXR<VM>>().unwrap();
    let config = MutatorConfig {
        allocator_mapping: &ALLOCATOR_MAPPING,
        space_mapping: Box::new({
            let mut vec = create_space_mapping(RESERVED_ALLOCATORS, false, mmtk.get_plan());
            vec.push((AllocatorSelector::Immix(0), &lxr.immix_space));
            vec.push((AllocatorSelector::LargeObject(0), &lxr.los));
            vec
        }),
        prepare_func: &lxr_mutator_prepare,
        release_func: &lxr_mutator_release,
    };

    Mutator {
        allocators: Allocators::<VM>::new(mutator_tls, mmtk, &config.space_mapping),
        barrier: Box::new(FieldBarrier::new(LXRFieldBarrierSemantics::new(
            mmtk,
            mutator_tls,
        ))),
        mutator_tls,
        config,
        plan: mmtk.get_plan(),
    }
}
