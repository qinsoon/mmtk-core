use super::defrag::Histogram;
use super::line::Line;
use super::ImmixSpace;
use crate::util::constants::*;
use crate::util::heap::blockpageresource::BlockPool;
use crate::util::heap::chunk_map::Chunk;
use crate::util::linear_scan::{Region, RegionIterator, UnstraddlableRegion};
use crate::util::metadata::side_metadata::*;
#[cfg(feature = "vo_bit")]
use crate::util::metadata::vo_bit;
#[cfg(feature = "object_pinning")]
use crate::util::metadata::MetadataSpec;
use crate::util::object_enum::BlockMayHaveObjects;
use crate::util::{Address, ObjectReference};
use crate::vm::*;
use bytemuck::NoUninit;
use std::sync::atomic::Ordering;

/// The block allocation state.
#[derive(Debug, PartialEq, Clone, Copy)]
pub enum BlockState {
    /// the block is not allocated.
    Unallocated,
    /// the block is a young block.
    Nursery,
    /// the block is allocated but not marked.
    Unmarked,
    /// the block is allocated and marked.
    Marked,
    /// RC mutator recycled blocks.
    Reusing,
    /// the block is marked as reusable.
    Reusable { unavailable_lines: u8 },
}

impl BlockState {
    /// Private constant
    const MARK_UNALLOCATED: u8 = 0;
    /// Private constant
    const MARK_UNMARKED: u8 = u8::MAX;
    /// Private constant
    const MARK_MARKED: u8 = u8::MAX - 1;
    const MARK_NURSERY: u8 = u8::MAX - 2;
    const MARK_REUSING: u8 = u8::MAX - 3;
}

impl From<u8> for BlockState {
    fn from(state: u8) -> Self {
        match state {
            Self::MARK_UNALLOCATED => BlockState::Unallocated,
            Self::MARK_UNMARKED => BlockState::Unmarked,
            Self::MARK_MARKED => BlockState::Marked,
            Self::MARK_NURSERY => BlockState::Nursery,
            Self::MARK_REUSING => BlockState::Reusing,
            unavailable_lines => BlockState::Reusable { unavailable_lines },
        }
    }
}

impl From<BlockState> for u8 {
    fn from(state: BlockState) -> Self {
        match state {
            BlockState::Unallocated => BlockState::MARK_UNALLOCATED,
            BlockState::Unmarked => BlockState::MARK_UNMARKED,
            BlockState::Marked => BlockState::MARK_MARKED,
            BlockState::Nursery => BlockState::MARK_NURSERY,
            BlockState::Reusing => BlockState::MARK_REUSING,
            BlockState::Reusable { unavailable_lines } => {
                assert_ne!(unavailable_lines, 0);
                u8::min(unavailable_lines, u8::MAX - 4)
            }
        }
    }
}

impl BlockState {
    /// Test if the block is reuasable.
    pub const fn is_reusable(&self) -> bool {
        matches!(self, BlockState::Reusable { .. })
    }
}

/// Data structure to reference an immix block.
#[repr(transparent)]
#[derive(Debug, Clone, Copy, PartialOrd, PartialEq, NoUninit)]
pub struct Block(Address);

impl Region for Block {
    #[cfg(not(feature = "immix_smaller_block"))]
    const LOG_BYTES: usize = 15;
    #[cfg(feature = "immix_smaller_block")]
    const LOG_BYTES: usize = 13;

    fn from_aligned_address(address: Address) -> Self {
        debug_assert!(address.is_aligned_to(Self::BYTES));
        Self(address)
    }

    fn start(&self) -> Address {
        self.0
    }
}

/// An objects cannot straddle multiple Immix blocks.
impl UnstraddlableRegion for Block {}

impl BlockMayHaveObjects for Block {
    fn may_have_objects(&self) -> bool {
        self.get_state() != BlockState::Unallocated
    }
}

impl Block {
    /// Log pages in block
    pub const LOG_PAGES: usize = Self::LOG_BYTES - LOG_BYTES_IN_PAGE as usize;
    /// Pages in block
    pub const PAGES: usize = 1 << Self::LOG_PAGES;
    /// Log lines in block
    pub const LOG_LINES: usize = Self::LOG_BYTES - Line::LOG_BYTES;
    /// Lines in block
    pub const LINES: usize = 1 << Self::LOG_LINES;

    /// Block defrag state table (side)
    pub const DEFRAG_STATE_TABLE: SideMetadataSpec =
        crate::util::metadata::side_metadata::spec_defs::IX_BLOCK_DEFRAG;

    /// Block mark table (side)
    pub const MARK_TABLE: SideMetadataSpec =
        crate::util::metadata::side_metadata::spec_defs::IX_BLOCK_MARK;
    pub const LOG_TABLE: SideMetadataSpec =
        crate::util::metadata::side_metadata::spec_defs::IX_BLOCK_LOG;
    pub const NURSERY_PROMOTION_STATE_TABLE: SideMetadataSpec =
        crate::util::metadata::side_metadata::spec_defs::NURSERY_PROMOTION_STATE;

    pub const ZERO: Self = Self(Address::ZERO);

    #[allow(unused)]
    pub fn is_zero(&self) -> bool {
        self.0.is_zero()
    }

    /// Get the chunk containing the block.
    pub fn chunk(&self) -> Chunk {
        Chunk::from_unaligned_address(self.0)
    }

    /// Get the address range of the block's line mark table.
    #[allow(clippy::assertions_on_constants)]
    pub fn line_mark_table(&self) -> MetadataByteArrayRef<{ Block::LINES }> {
        debug_assert!(!super::BLOCK_ONLY);
        MetadataByteArrayRef::<{ Block::LINES }>::new(&Line::MARK_TABLE, self.start(), Self::BYTES)
    }

    /// Get block mark state.
    pub fn get_state(&self) -> BlockState {
        let byte = Self::MARK_TABLE.load_atomic::<u8>(self.start(), Ordering::SeqCst);
        byte.into()
    }

    /// Set block mark state.
    pub fn set_state(&self, state: BlockState) {
        let state = u8::from(state);
        Self::MARK_TABLE.store_atomic::<u8>(self.start(), state, Ordering::SeqCst);
    }

    /// Set block mark state.
    pub fn fetch_update_state(
        &self,
        mut f: impl FnMut(BlockState) -> Option<BlockState>,
    ) -> Result<BlockState, BlockState> {
        Self::MARK_TABLE
            .fetch_update_atomic::<u8, _>(self.start(), Ordering::SeqCst, Ordering::SeqCst, |s| {
                f(s.into()).map(u8::from)
            })
            .map(|x| x.into())
            .map_err(|x| x.into())
    }

    pub fn attempt_dealloc(&self, ignore_reusing_blocks: bool) -> bool {
        self.fetch_update_state(|s| {
            if (ignore_reusing_blocks && s == BlockState::Reusing) || s == BlockState::Unallocated {
                None
            } else {
                Some(BlockState::Unallocated)
            }
        })
        .is_ok()
    }

    // Defrag byte

    const DEFRAG_SOURCE_STATE: u8 = u8::MAX;

    /// Test if the block is marked for defragmentation.
    pub fn is_defrag_source(&self) -> bool {
        let byte = Self::DEFRAG_STATE_TABLE.load_atomic::<u8>(self.start(), Ordering::SeqCst);
        // The byte should be 0 (not defrag source) or 255 (defrag source) if this is a major defrag GC, as we set the values in PrepareBlockState.
        // But it could be any value in a nursery GC.
        byte == Self::DEFRAG_SOURCE_STATE
    }

    pub fn in_defrag_block(o: ObjectReference) -> bool {
        Block::containing(o).is_defrag_source()
    }

    pub fn address_in_defrag_block(a: Address) -> bool {
        Block::from_unaligned_address(a).is_defrag_source()
    }

    /// Mark the block for defragmentation.
    pub fn set_as_defrag_source(&self, defrag: bool) {
        let byte = if defrag { Self::DEFRAG_SOURCE_STATE } else { 0 };
        Self::DEFRAG_STATE_TABLE.store_atomic::<u8>(self.start(), byte, Ordering::SeqCst);
    }

    /// Record the number of holes in the block.
    pub fn set_holes(&self, holes: usize) {
        Self::DEFRAG_STATE_TABLE.store_atomic::<u8>(self.start(), holes as u8, Ordering::SeqCst);
    }

    /// Get the number of holes.
    pub fn get_holes(&self) -> usize {
        let byte = Self::DEFRAG_STATE_TABLE.load_atomic::<u8>(self.start(), Ordering::SeqCst);
        debug_assert_ne!(byte, Self::DEFRAG_SOURCE_STATE);
        byte as usize
    }

    /// Initialize a clean block after acquired from page-resource. `rc_enabled` distinguishes
    /// LXR's ref-counting block-state protocol from the generic tracing one; it is a plain flag
    /// (not a space reference) because this method is shared by both `ImmixSpace` and
    /// `LXRSpace`, and neither regime needs anything else from the space here.
    pub fn init(&self, copy: bool, reuse: bool, rc_enabled: bool) {
        if rc_enabled {
            if !reuse {
                debug_assert_eq!(self.get_state(), BlockState::Unallocated);
            }
            self.clear_in_place_promoted();
            if !copy && reuse {
                self.set_state(BlockState::Reusing);
                debug_assert!(!self.is_defrag_source());
            } else if copy {
                if reuse {
                    debug_assert!(!self.is_defrag_source());
                }
                self.set_state(BlockState::Unmarked);
                self.set_as_defrag_source(false);
            } else {
                self.set_state(BlockState::Nursery);
                self.set_as_defrag_source(false);
            }
        } else {
            self.set_state(if copy {
                BlockState::Marked
            } else {
                BlockState::Unmarked
            });
            if !reuse {
                Self::DEFRAG_STATE_TABLE.store_atomic::<u8>(self.start(), 0, Ordering::SeqCst);
            }
        }
    }

    /// Deinitalize a block before releasing.
    pub fn deinit(&self, rc_enabled: bool) {
        self.set_state(BlockState::Unallocated);
        if rc_enabled {
            self.set_as_defrag_source(false);
        }
    }

    pub fn start_line(&self) -> Line {
        Line::from_aligned_address(self.start())
    }

    pub fn end_line(&self) -> Line {
        Line::from_aligned_address(self.end())
    }

    /// Get the range of lines within the block.
    #[allow(clippy::assertions_on_constants)]
    pub fn lines(&self) -> RegionIterator<Line> {
        debug_assert!(!super::BLOCK_ONLY);
        RegionIterator::<Line>::new(self.start_line(), self.end_line())
    }

    pub fn set_as_in_place_promoted(&self) {
        if self.is_in_place_promoted() {
            return;
        }
        unsafe { Self::NURSERY_PROMOTION_STATE_TABLE.store(self.start(), 1u8) };
    }

    pub fn is_in_place_promoted(&self) -> bool {
        Self::NURSERY_PROMOTION_STATE_TABLE.load_atomic::<u8>(self.start(), Ordering::Relaxed) != 0
    }

    pub fn clear_in_place_promoted(&self) {
        unsafe { Self::NURSERY_PROMOTION_STATE_TABLE.store(self.start(), 0u8) };
    }

    pub fn clear_field_unlog_table<VM: VMBinding>(&self) {
        VM::VMObjectModel::GLOBAL_FIELD_UNLOG_BIT_SPEC
            .as_spec()
            .extract_side_spec()
            .bzero_metadata(self.start(), Block::BYTES);
    }

    /// Sweep this block.
    pub fn sweep<VM: VMBinding>(
        &self,
        space: &ImmixSpace<VM>,
        mark_histogram: &mut Histogram,
        line_mark_state: Option<u8>,
    ) -> BlockSweepResult {
        self.set_as_defrag_source(false);
        if super::BLOCK_ONLY {
            match self.get_state() {
                BlockState::Unallocated => unreachable!("Must not sweep unallocated block."),
                BlockState::Unmarked => {
                    #[cfg(feature = "vo_bit")]
                    vo_bit::helper::on_region_swept::<VM, _>(self, false);

                    // If the pin bit is not on the side, we cannot bulk zero.
                    // We shouldn't need to clear it here in that case, since the pin bit
                    // should be overwritten at each object allocation. The same applies below
                    // when we are sweeping on a line granularity.
                    #[cfg(feature = "object_pinning")]
                    if let MetadataSpec::OnSide(side) = *VM::VMObjectModel::LOCAL_PINNING_BIT_SPEC {
                        side.bzero_metadata(self.start(), Block::BYTES);
                    }

                    // Release the block if it is allocated but not marked by the current GC.
                    space.release_block(*self, false);
                    BlockSweepResult::Swept
                }
                BlockState::Marked => {
                    #[cfg(feature = "vo_bit")]
                    vo_bit::helper::on_region_swept::<VM, _>(self, true);

                    // The block is live.
                    BlockSweepResult::NoReuse
                }
                _ => unreachable!(),
            }
        } else {
            // Calculate number of marked lines and holes.
            let mut marked_lines = 0;
            let mut holes = 0;
            let mut prev_line_is_marked = true;
            let line_mark_state = line_mark_state.unwrap();

            for line in self.lines() {
                if line.is_marked(line_mark_state) {
                    marked_lines += 1;
                    prev_line_is_marked = true;
                } else {
                    if prev_line_is_marked {
                        holes += 1;
                    }
                    // We need to clear the line mark state at least twice in every 128 GC
                    // otherwise, the line mark state of the last GC will stick around
                    if line_mark_state > Line::MAX_MARK_STATE - 2 {
                        line.mark(0);
                    }
                    #[cfg(feature = "immix_zero_on_release")]
                    crate::util::memory::zero(line.start(), Line::BYTES);

                    // We need to clear the pin bit if it is on the side, as this line can be reused
                    #[cfg(feature = "object_pinning")]
                    if let MetadataSpec::OnSide(side) = *VM::VMObjectModel::LOCAL_PINNING_BIT_SPEC {
                        side.bzero_metadata(line.start(), Line::BYTES);
                    }

                    prev_line_is_marked = false;
                }
            }

            if marked_lines == 0 {
                #[cfg(feature = "vo_bit")]
                vo_bit::helper::on_region_swept::<VM, _>(self, false);

                // Release the block if non of its lines are marked.
                space.release_block(*self, false);
                BlockSweepResult::Swept
            } else {
                // There are some marked lines. Keep the block live.
                let is_reusable = marked_lines != Block::LINES;
                if is_reusable {
                    // There are holes. Mark the block as reusable.
                    self.set_state(BlockState::Reusable {
                        unavailable_lines: usize::min(marked_lines, u8::MAX as usize) as _,
                    });
                    space.reusable_blocks.push(*self)
                } else {
                    // Clear mark state.
                    self.set_state(BlockState::Unmarked);
                }
                // Update mark_histogram
                mark_histogram[holes] += marked_lines;
                // Record number of holes in block side metadata.
                self.set_holes(holes);

                #[cfg(feature = "vo_bit")]
                vo_bit::helper::on_region_swept::<VM, _>(self, true);

                if is_reusable {
                    BlockSweepResult::Reused
                } else {
                    BlockSweepResult::NoReuse
                }
            }
        }
    }

    /// Clear VO bits metadata for unmarked regions.
    /// This is useful for clearing VO bits during nursery GC for StickyImmix
    /// at which time young objects (allocated in unmarked regions) may die
    /// but we always consider old objects (in marked regions) as live.
    #[cfg(feature = "vo_bit")]
    pub fn clear_vo_bits_for_unmarked_regions(&self, line_mark_state: Option<u8>) {
        match line_mark_state {
            None => {
                match self.get_state() {
                    BlockState::Unmarked => {
                        // It may contain young objects.  Clear it.
                        vo_bit::bzero_vo_bit(self.start(), Self::BYTES);
                    }
                    BlockState::Marked => {
                        // It contains old objects.  Skip it.
                    }
                    _ => unreachable!(),
                }
            }
            Some(state) => {
                // With lines.
                for line in self.lines() {
                    if !line.is_marked(state) {
                        // It may contain young objects.  Clear it.
                        vo_bit::bzero_vo_bit(line.start(), Line::BYTES);
                    }
                }
            }
        }
    }
}

/// A non-block single-linked list to store blocks.
pub struct ReusableBlockPool {
    queue: BlockPool<Block>,
    num_workers: usize,
}

impl ReusableBlockPool {
    /// Create empty block list
    pub fn new(num_workers: usize) -> Self {
        Self {
            queue: BlockPool::new(num_workers),
            num_workers,
        }
    }

    /// Get number of blocks in this list.
    pub fn len(&self) -> usize {
        self.queue.len()
    }

    /// Add a block to the list.
    pub fn push(&self, block: Block) {
        self.queue.push(block)
    }

    /// Pop a block out of the list.
    pub fn pop(&self) -> Option<Block> {
        self.queue.pop()
    }

    /// Clear the list.
    pub fn reset(&mut self) {
        self.queue = BlockPool::new(self.num_workers);
    }

    /// Iterate all the blocks in the queue. Call the visitor for each reported block.
    pub fn iterate_blocks(&self, mut f: impl FnMut(Block)) {
        self.queue.iterate_blocks(&mut f);
    }

    /// Flush the block queue
    pub fn flush_all(&self) {
        self.queue.flush_all();
    }
}

/// The result of sweeping a block.  Mainly used for statistics.
pub enum BlockSweepResult {
    /// The block is completely free.
    Swept,
    /// The block is partially free, and is reused.
    Reused,
    /// The block cannot be reused.  When [`super::BLOCK_ONLY`] is true, it is returned whenever a
    /// block is not completely free.  Otherwise it is returned when a block is full.
    NoReuse,
}
