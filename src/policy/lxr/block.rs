//! LXR-only extensions to the generic [`Block`] type.
//!
//! These are RC-only behaviors (RC-mutator-recycled block state, RC-based hole search, RC
//! nursery/mature sweeping, the block-level log bit) that used to be plain inherent methods on
//! `Block`, reachable from any Immix-family plan. They're gathered behind [`LXRBlockExt`] and
//! implemented here instead, so a non-LXR plan (Immix, GenImmix, StickyImmix, ConcurrentImmix)
//! has no path to them without deliberately importing this trait.
//!
//! A few RC-only-looking members stay inherent on `Block` (in `policy::immix::block`) rather than
//! moving here, because generic/shared code depends on them directly: the "in-place-promoted"
//! trio (the shared `Block::init` calls `clear_in_place_promoted` in its RC branch) and
//! `clear_field_unlog_table` (the shared `ImmixSpace::release_block` calls it when asked to zero
//! the unlog table). Moving those would make the generic `policy::immix` module depend on this
//! LXR-specific one.

use crate::policy::immix::block::{Block, BlockState};
#[cfg(feature = "vo_bit")]
use crate::policy::immix::line::Line;
use crate::policy::immix::line::RCArray;
use crate::policy::lxr::LXRSpace;
use crate::util::linear_scan::Region;
use crate::util::metadata::side_metadata::*;
use crate::vm::*;
use std::sync::atomic::Ordering;

pub trait LXRBlockExt {
    fn calc_dead_lines(&self) -> usize;
    fn clear_rc_table(&self);
    fn clear_striddle_table(&self);
    fn clear_mark_table<VM: VMBinding>(&self);
    fn initialize_mark_table_as_marked<VM: VMBinding>(&self);
    fn log(&self) -> bool;
    fn unlog(&self);
    fn initialize_field_unlog_table_as_unlogged<VM: VMBinding>(&self);
    fn rc_dead(&self) -> bool;
    fn has_holes(&self) -> bool;
    fn attempt_mutator_reuse(&self) -> bool;
    fn rc_sweep_nursery<VM: VMBinding>(&self, space: &LXRSpace<VM>) -> bool;
    fn rc_sweep_mature<VM: VMBinding>(&self, space: &LXRSpace<VM>, defrag: bool) -> bool;
}

impl LXRBlockExt for Block {
    fn calc_dead_lines(&self) -> usize {
        let mut dead_lines = 0;
        let rc_array = RCArray::of(*self);
        for i in 0..Self::LINES {
            if rc_array.is_dead(i) {
                dead_lines += 1;
            }
        }
        dead_lines
    }

    fn clear_rc_table(&self) {
        crate::util::rc::RC_TABLE.bzero_metadata(self.start(), Block::BYTES);
    }

    fn clear_striddle_table(&self) {
        crate::util::rc::RC_STRADDLE_LINES.bzero_metadata(self.start(), Block::BYTES);
    }

    fn clear_mark_table<VM: VMBinding>(&self) {
        VM::VMObjectModel::LOCAL_MARK_BIT_SPEC
            .extract_side_spec()
            .bzero_metadata(self.start(), Self::BYTES);
    }

    fn initialize_mark_table_as_marked<VM: VMBinding>(&self) {
        let meta = VM::VMObjectModel::LOCAL_MARK_BIT_SPEC.extract_side_spec();
        let start: *mut u8 = address_to_meta_address(meta, self.start()).to_mut_ptr();
        let limit: *mut u8 = address_to_meta_address(meta, self.end()).to_mut_ptr();
        unsafe {
            let bytes = limit.offset_from(start) as usize;
            std::ptr::write_bytes(start, 0xffu8, bytes);
        }
    }

    fn log(&self) -> bool {
        loop {
            let old_value: u8 = Self::LOG_TABLE.load_atomic(self.start(), Ordering::Relaxed);
            if old_value == 1 {
                return false;
            }
            if Self::LOG_TABLE
                .compare_exchange_atomic(self.start(), 0u8, 1u8, Ordering::SeqCst, Ordering::SeqCst)
                .is_ok()
            {
                return true;
            }
        }
    }

    fn unlog(&self) {
        Self::LOG_TABLE.store_atomic(self.start(), 0u8, Ordering::Relaxed);
    }

    fn initialize_field_unlog_table_as_unlogged<VM: VMBinding>(&self) {
        let meta = *VM::VMObjectModel::GLOBAL_FIELD_UNLOG_BIT_SPEC
            .as_spec()
            .extract_side_spec();
        let start: *mut u8 = address_to_meta_address(&meta, self.start()).to_mut_ptr();
        let limit: *mut u8 = address_to_meta_address(&meta, self.end()).to_mut_ptr();
        unsafe {
            let bytes = limit.offset_from(start) as usize;
            std::ptr::write_bytes(start, 0xffu8, bytes);
        }
    }

    #[allow(clippy::assertions_on_constants)]
    fn rc_dead(&self) -> bool {
        type UInt = u128;
        const LOG_BITS_IN_UINT: usize =
            (std::mem::size_of::<UInt>() << 3).trailing_zeros() as usize;
        debug_assert!(
            Self::LOG_BYTES - crate::util::rc::LOG_MIN_OBJECT_SIZE
                + crate::util::rc::LOG_REF_COUNT_BITS
                >= LOG_BITS_IN_UINT
        );
        let start =
            address_to_meta_address(&crate::util::rc::RC_TABLE, self.start()).to_ptr::<UInt>();
        let limit =
            address_to_meta_address(&crate::util::rc::RC_TABLE, self.end()).to_ptr::<UInt>();
        let rc_table = unsafe { std::slice::from_raw_parts(start, limit.offset_from(start) as _) };
        for x in rc_table {
            if *x != 0 {
                return false;
            }
        }
        true
    }

    fn has_holes(&self) -> bool {
        let rc_array = RCArray::of(*self);
        let mut found_free_line = false;
        let mut free_lines = 0;
        for i in 0..Self::LINES {
            if rc_array.is_dead(i) {
                if i == 0 || found_free_line {
                    free_lines += 1
                } else if !found_free_line {
                    found_free_line = true;
                }
                if free_lines > 0 {
                    return true;
                }
            } else {
                free_lines = 0;
                found_free_line = false;
            }
        }
        false
    }

    fn attempt_mutator_reuse(&self) -> bool {
        self.fetch_update_state(|s| {
            if s.is_reusable() {
                Some(BlockState::Reusing)
            } else {
                None
            }
        })
        .is_ok()
    }

    fn rc_sweep_nursery<VM: VMBinding>(&self, space: &LXRSpace<VM>) -> bool {
        let is_in_place_promoted = self.is_in_place_promoted();
        self.clear_in_place_promoted();
        if is_in_place_promoted {
            self.set_state(BlockState::Reusable {
                unavailable_lines: 1 as _,
            });

            // Bulk clear the VO bits of reusable (unmarked) lines.
            // Lines that are not marked may contain nursery objects that have never received any inc,
            // and their VO bits need to be cleared before the lines can be reused.
            #[cfg(feature = "vo_bit")]
            {
                let rc_array = RCArray::of(*self);

                for (i, line) in self.lines().enumerate() {
                    if rc_array.is_dead(i) {
                        crate::util::metadata::vo_bit::bzero_vo_bit(line.start(), Line::BYTES);
                    }
                }
            }

            space.inner().reusable_blocks.push(*self);
            false
        } else {
            debug_assert!(self.rc_dead(), "{:?} has non-zero rc value", self);
            debug_assert_ne!(self.get_state(), BlockState::Unallocated);

            // Bulk clear the VO bits of the entire block.
            // This block may contain nursery objects that have never received any inc,
            // and their VO bits need to be cleared before the block can be reused.
            #[cfg(feature = "vo_bit")]
            crate::util::metadata::vo_bit::bzero_vo_bit(self.start(), Self::BYTES);

            space.inner().release_block(*self, false);
            true
        }
    }

    fn rc_sweep_mature<VM: VMBinding>(&self, space: &LXRSpace<VM>, defrag: bool) -> bool {
        if self.get_state() == BlockState::Unallocated || self.get_state() == BlockState::Nursery {
            return false;
        }
        if defrag || self.rc_dead() {
            if self.attempt_dealloc(true) {
                // Bulk clear the VO bits of the entire block.
                // Dec operations may reduce some object's RC to 0,
                // at which time their VO bits are cleared, too.
                // But some lines may also contain objects that have never received any inc,
                // and their VO bits need to be cleared before the block can be reused.
                #[cfg(feature = "vo_bit")]
                crate::util::metadata::vo_bit::bzero_vo_bit(self.start(), Self::BYTES);

                space.inner().release_block(*self, true);
                return true;
            }
        } else if !crate::policy::immix::BLOCK_ONLY {
            // See the caller of this function.
            // At least one object is dead in the block.
            let add_as_reusable = {
                let has_holes = self.has_holes();
                self.fetch_update_state(|s| {
                    if s == BlockState::Reusing
                        || s == BlockState::Unallocated
                        || s.is_reusable()
                        || !has_holes
                    {
                        None
                    } else {
                        Some(BlockState::Reusable {
                            unavailable_lines: 1 as _,
                        })
                    }
                })
                .is_ok()
            };
            if add_as_reusable {
                // Bulk clear the VO bits of reusable (unmarked) lines.
                // Dec operations may reduce some object's RC to 0,
                // at which time their VO bits are cleared, too.
                // But some lines may also contain objects that have never received any inc,
                // and their VO bits need to be cleared before the block can be reused.
                #[cfg(feature = "vo_bit")]
                {
                    let rc_array = RCArray::of(*self);

                    for (i, line) in self.lines().enumerate() {
                        if rc_array.is_dead(i) {
                            crate::util::metadata::vo_bit::bzero_vo_bit(line.start(), Line::BYTES);
                        }
                    }
                }
                space.inner().reusable_blocks.push(*self);
            }
        }
        false
    }
}
