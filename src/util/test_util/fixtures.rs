// Some tests are conditionally compiled. So not all the code in this module will be used. We simply allow dead code in this module.
#![allow(dead_code)]

use atomic_refcell::AtomicRefCell;
use std::sync::Mutex;
use std::sync::Once;

use crate::MMTKBuilder;
use crate::util::{ObjectReference, VMMutatorThread, VMThread};
use crate::AllocationSemantics;
use crate::MMTK;
use crate::memory_manager;
use crate::util::test_util::mock_vm::MockVM;

pub trait FixtureContent {
    fn create() -> Self;
}

pub struct Fixture<T: FixtureContent> {
    content: AtomicRefCell<Option<Box<T>>>,
    once: Once,
}

unsafe impl<T: FixtureContent> Sync for Fixture<T> {}

impl<T: FixtureContent> Fixture<T> {
    pub fn new() -> Self {
        Self {
            content: AtomicRefCell::new(None),
            once: Once::new(),
        }
    }

    pub fn with_fixture<F: Fn(&T)>(&self, func: F) {
        self.once.call_once(|| {
            let content = Box::new(T::create());
            let mut borrow = self.content.borrow_mut();
            *borrow = Some(content);
        });
        let borrow = self.content.borrow();
        func(borrow.as_ref().unwrap())
    }
}

impl<T: FixtureContent> Default for Fixture<T> {
    fn default() -> Self {
        Self::new()
    }
}

/// SerialFixture ensures all `with_fixture()` calls will be executed serially.
pub struct SerialFixture<T: FixtureContent> {
    content: Mutex<Option<Box<T>>>,
}

impl<T: FixtureContent> SerialFixture<T> {
    pub fn new() -> Self {
        Self {
            content: Mutex::new(None),
        }
    }

    pub fn with_fixture<F: Fn(&T)>(&self, func: F) {
        let mut c = self.content.lock().unwrap();
        if c.is_none() {
            *c = Some(Box::new(T::create()));
        }
        func(c.as_ref().unwrap())
    }

    pub fn with_fixture_mut<F: Fn(&mut T)>(&self, func: F) {
        let mut c = self.content.lock().unwrap();
        if c.is_none() {
            *c = Some(Box::new(T::create()));
        }
        func(c.as_mut().unwrap())
    }

    pub fn with_fixture_expect_benign_panic<
        F: Fn(&T) + std::panic::UnwindSafe + std::panic::RefUnwindSafe,
    >(
        &self,
        func: F,
    ) {
        let res = {
            // The lock will be dropped at the end of the block. So panic won't poison the lock.
            let mut c = self.content.lock().unwrap();
            if c.is_none() {
                *c = Some(Box::new(T::create()));
            }

            std::panic::catch_unwind(|| func(c.as_ref().unwrap()))
        };
        // We do not hold the lock now. It is safe to resume now.
        if let Err(e) = res {
            std::panic::resume_unwind(e);
        }
    }
}

impl<T: FixtureContent> Default for SerialFixture<T> {
    fn default() -> Self {
        Self::new()
    }
}

pub struct MMTKSingleton {
    pub mmtk: &'static MMTK<MockVM>,
}

impl FixtureContent for MMTKSingleton {
    fn create() -> Self {
        // 1MB heap
        const MB: usize = 1024 * 1024;
        let mut builder = MMTKBuilder::new();
        builder.options.gc_trigger.set(crate::util::options::GCTriggerSelector::FixedHeapSize(MB));
        let mmtk = memory_manager::mmtk_init(&builder);
        let mmtk_ptr = Box::into_raw(mmtk);
        let mmtk_static: &'static MMTK<MockVM> = unsafe { &*mmtk_ptr };
        memory_manager::initialize_collection(mmtk_static, VMThread::UNINITIALIZED);
        // Make sure GC does not run during test.
        memory_manager::disable_collection(mmtk_static);

        MMTKSingleton {
            mmtk: mmtk_static,
        }
    }
}


use crate::plan::Mutator;

pub struct MutatorFixture {
    pub mmtk: &'static MMTK<MockVM>,
    pub mutator: Box<Mutator<MockVM>>,
}

impl FixtureContent for MutatorFixture {
    fn create() -> Self {
        const MB: usize = 1024 * 1024;
        Self::create_with_heapsize(MB)
    }
}

impl MutatorFixture {
    pub fn create_with_heapsize(size: usize) -> Self {
        let mut builder = MMTKBuilder::new();
        builder.options.gc_trigger.set(crate::util::options::GCTriggerSelector::FixedHeapSize(size));
        let mmtk = memory_manager::mmtk_init(&builder);
        let mmtk_ptr = Box::into_raw(mmtk);
        let mmtk_static: &'static MMTK<MockVM> = unsafe { &*mmtk_ptr };
        memory_manager::initialize_collection(mmtk_static, VMThread::UNINITIALIZED);
        // Make sure GC does not run during test.
        memory_manager::disable_collection(mmtk_static);
        let mutator = memory_manager::bind_mutator(mmtk_static, VMMutatorThread(VMThread::UNINITIALIZED));
        Self {
            mmtk: mmtk_static,
            mutator,
        }
    }
}

unsafe impl Send for MutatorFixture {}
