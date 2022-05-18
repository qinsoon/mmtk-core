//! The fundamental mechanism for performing a transitive closure over an object graph.

use std::mem;

use crate::scheduler::gc_work::ProcessEdgesWork;
use crate::scheduler::{GCWorker, WorkBucketStage};
use crate::util::{Address, ObjectReference};
use crate::vm::EdgeVisitor;

/// This trait is the fundamental mechanism for performing a
/// transitive closure over an object graph.
pub trait TransitiveClosure {
    // The signature of this function changes during the port
    // because the argument `ObjectReference source` is never used in the original version
    // See issue #5
    fn process_node(&mut self, object: ObjectReference);
}

impl<T: ProcessEdgesWork> TransitiveClosure for T {
    #[inline]
    fn process_node(&mut self, object: ObjectReference) {
        ProcessEdgesWork::process_node(self, object);
    }
}

// use crate::util::reference_processor::ReferenceBuffer;

/// A transitive closure visitor to collect all the edges of an object.
pub struct ObjectsClosure<'a, E: ProcessEdgesWork> {
    buffer: Vec<Address>,
    // weak_ref_buffer: ReferenceBuffer,
    worker: &'a mut GCWorker<E::VM>,
}

impl<'a, E: ProcessEdgesWork> ObjectsClosure<'a, E> {
    pub fn new(worker: &'a mut GCWorker<E::VM>) -> Self {
        Self {
            buffer: vec![],
            // weak_ref_buffer: ReferenceBuffer::new(),
            worker,
        }
    }

    fn flush(&mut self) {
        let mut new_edges = Vec::new();
        mem::swap(&mut new_edges, &mut self.buffer);
        self.worker.add_work(
            WorkBucketStage::Closure,
            E::new(new_edges, false, self.worker.mmtk),
        );
        // self.weak_ref_buffer.flush(&self.worker.mmtk.reference_processors);
    }
}

impl<'a, E: ProcessEdgesWork> EdgeVisitor for ObjectsClosure<'a, E> {
    #[inline(always)]
    fn visit_edge(&mut self, slot: Address) {
        if self.buffer.is_empty() {
            self.buffer.reserve(E::CAPACITY);
        }
        self.buffer.push(slot);
        if self.buffer.len() >= E::CAPACITY {
            let mut new_edges = Vec::new();
            mem::swap(&mut new_edges, &mut self.buffer);
            self.worker.add_work(
                WorkBucketStage::Closure,
                E::new(new_edges, false, self.worker.mmtk),
            );
        }
    }

    #[inline(always)]
    fn visit_soft_ref(&mut self, reff: ObjectReference) {
        // self.weak_ref_buffer.add_soft_ref(reff)
        self.worker.reference_buffer.add_soft_ref(reff)
    }
    #[inline(always)]
    fn visit_weak_ref(&mut self, reff: ObjectReference) {
        // self.weak_ref_buffer.add_weak_ref(reff)
        self.worker.reference_buffer.add_weak_ref(reff)
    }
    #[inline(always)]
    fn visit_phantom_ref(&mut self, reff: ObjectReference) {
        // self.weak_ref_buffer.add_phantom_ref(reff)
        self.worker.reference_buffer.add_phantom_ref(reff)
    }
}

impl<'a, E: ProcessEdgesWork> Drop for ObjectsClosure<'a, E> {
    #[inline(always)]
    fn drop(&mut self) {
        self.flush();
    }
}
