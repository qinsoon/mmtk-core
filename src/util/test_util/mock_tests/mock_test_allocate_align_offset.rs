// GITHUB-CI: MMTK_PLAN=all

use lazy_static::lazy_static;

use crate::util::test_util::fixtures::*;
use crate::util::test_util::mock_vm::*;
use crate::plan::AllocationSemantics;
use crate::memory_manager;
use crate::vm::VMBinding;

lazy_static! {
    static ref MUTATOR: SerialFixture<MutatorFixture> = SerialFixture::new();
}

#[test]
pub fn allocate_alignment() {
    MUTATOR.with_fixture_mut(|fixture| {
        let min = MockVM::MIN_ALIGNMENT;
        let max = MockVM::MAX_ALIGNMENT;
        info!("Allowed alignment between {} and {}", min, max);
        let mut align = min;
        while align <= max {
            info!("Test allocation with alignment {}", align);
            let addr = memory_manager::alloc(&mut fixture.mutator, 8, align, 0, AllocationSemantics::Default);
            assert!(
                addr.is_aligned_to(align),
                "Expected allocation alignment {}, returned address is {:?}",
                align,
                addr
            );
            align *= 2;
        }
    })
}

#[test]
pub fn allocate_offset() {
    MUTATOR.with_fixture_mut(|fixture| {
        const OFFSET: usize = 4;
        let min = MockVM::MIN_ALIGNMENT;
        let max = MockVM::MAX_ALIGNMENT;
        info!("Allowed alignment between {} and {}", min, max);
        let mut align = min;
        while align <= max {
            info!(
                "Test allocation with alignment {} and offset {}",
                align, OFFSET
            );
            let addr = memory_manager::alloc(
                &mut fixture.mutator,
                8,
                align,
                OFFSET,
                AllocationSemantics::Default,
            );
            assert!(
                (addr + OFFSET).is_aligned_to(align),
                "Expected allocation alignment {}, returned address is {:?}",
                align,
                addr
            );
            align *= 2;
        }
    })
}
