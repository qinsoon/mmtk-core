use std::sync::Arc;

use crate::policy::largeobjectspace::{LargeObjectSpace, LargeObjectSpaceExt};
use crate::policy::space::Space;
use crate::util::alloc::{allocator, Allocator};
use crate::util::opaque_pointer::*;
use crate::util::Address;
use crate::vm::VMBinding;

use super::allocator::AllocatorContext;

/// An allocator that only allocates at page granularity.
/// This is intended for large objects.
#[repr(C)]
pub struct LargeObjectAllocator<VM: VMBinding> {
    /// [`VMThread`] associated with this allocator instance
    pub tls: VMThread,
    /// [`Space`](src/policy/space/Space) instance associated with this allocator instance.
    space: &'static dyn LargeObjectSpaceExt<VM>,
    context: Arc<AllocatorContext<VM>>,
}

impl<VM: VMBinding> Allocator<VM> for LargeObjectAllocator<VM> {
    fn get_tls(&self) -> VMThread {
        self.tls
    }

    fn get_context(&self) -> &AllocatorContext<VM> {
        &self.context
    }

    fn get_space(&self) -> &'static dyn Space<VM> {
        self.space.as_space()
    }

    fn does_thread_local_allocation(&self) -> bool {
        false
    }

    fn alloc(&mut self, size: usize, align: usize, offset: usize) -> Address {
        let cell: Address = self.alloc_slow(size, align, offset);
        // We may get a null ptr from alloc due to the VM being OOM
        if !cell.is_zero() {
            allocator::align_allocation::<VM>(cell, align, offset)
        } else {
            cell
        }
    }

    fn alloc_slow_once(&mut self, size: usize, align: usize, _offset: usize) -> Address {
        let maxbytes = allocator::get_maximum_aligned_size::<VM>(size, align);
        let pages = crate::util::conversions::bytes_to_pages_up(maxbytes);

        if self.handle_obvious_oom_request(
            self.tls,
            pages << crate::util::constants::LOG_BYTES_IN_PAGE,
        ) {
            return Address::ZERO;
        }

        self.space
            .allocate_pages(self.tls, pages, self.get_context().get_alloc_options())
    }
}

impl<VM: VMBinding> LargeObjectAllocator<VM> {
    pub(crate) fn new(
        tls: VMThread,
        space: &'static dyn Space<VM>,
        context: Arc<AllocatorContext<VM>>,
    ) -> Self {
        // `LargeObjectAllocator` targets either the generic tracing `LargeObjectSpace` or LXR's
        // ref-counting `LXRLargeObjectSpace`. Both implement `LargeObjectSpaceExt`; resolve the
        // concrete type once here so the rest of the allocator is written purely against the
        // trait (mirrors `ImmixAllocator::new`).
        let space: &'static dyn LargeObjectSpaceExt<VM> =
            if let Some(s) = space.downcast_ref::<LargeObjectSpace<VM>>() {
                s
            } else if let Some(s) =
                space.downcast_ref::<crate::policy::lxr::LXRLargeObjectSpace<VM>>()
            {
                s
            } else {
                panic!(
                    "LargeObjectAllocator::new: space {} is neither LargeObjectSpace nor LXRLargeObjectSpace",
                    space.get_name()
                )
            };
        LargeObjectAllocator {
            tls,
            space,
            context,
        }
    }
}
