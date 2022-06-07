use std::collections::HashSet;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;
use std::sync::Mutex;
use std::vec::Vec;

use crate::scheduler::ProcessEdgesWork;
use crate::util::ObjectReference;
use crate::util::VMWorkerThread;
use crate::vm::ReferenceGlue;
use crate::vm::VMBinding;

/// Holds all reference processors for each weak reference Semantics.
/// Currently this is based on Java's weak reference semantics (soft/weak/phantom).
/// We should make changes to make this general rather than Java specific.
pub struct ReferenceProcessors {
    soft: ReferenceProcessor,
    weak: ReferenceProcessor,
    phantom: ReferenceProcessor,
}

impl ReferenceProcessors {
    pub fn new() -> Self {
        ReferenceProcessors {
            soft: ReferenceProcessor::new(Semantics::SOFT),
            weak: ReferenceProcessor::new(Semantics::WEAK),
            phantom: ReferenceProcessor::new(Semantics::PHANTOM),
        }
    }

    pub fn get(&self, semantics: Semantics) -> &ReferenceProcessor {
        match semantics {
            Semantics::SOFT => &self.soft,
            Semantics::WEAK => &self.weak,
            Semantics::PHANTOM => &self.phantom,
        }
    }

    pub fn add_soft_candidate(&self, reff: ObjectReference) {
        trace!("Add soft candidate: {}", reff);
        self.soft.add_candidate(reff);
    }

    pub fn add_soft_candidates(&self, refs: &[ObjectReference]) {
        trace!("Add soft candidates: {:?}", refs);
        self.soft.add_candidates(refs);
    }

    pub fn add_weak_candidate(&self, reff: ObjectReference) {
        trace!("Add weak candidate: {}", reff);
        self.weak.add_candidate(reff);
    }

    pub fn add_weak_candidates(&self, refs: &[ObjectReference]) {
        trace!("Add weak candidates: {:?}", refs);
        self.weak.add_candidates(refs);
    }

    pub fn add_phantom_candidate(&self, reff: ObjectReference) {
        trace!("Add phantom candidate: {}", reff);
        self.phantom.add_candidate(reff);
    }

    pub fn add_phantom_candidates(&self, refs: &[ObjectReference]) {
        trace!("Add phantom candidates: {:?}", refs);
        self.phantom.add_candidates(refs);
    }

    /// This will invoke enqueue for each reference processor, which will
    /// call back to the VM to enqueue references whose referents are cleared
    /// in this GC.
    pub fn enqueue_refs<VM: VMBinding>(&self, tls: VMWorkerThread) {
        self.soft.enqueue::<VM>(tls);
        self.weak.enqueue::<VM>(tls);
        self.phantom.enqueue::<VM>(tls);
    }

    /// A separate reference forwarding step. Normally when we scan refs, we deal with forwarding.
    /// However, for some plans like mark compact, at the point we do ref scanning, we do not know
    /// the forwarding addresses yet, thus we cannot do forwarding during scan refs. And for those
    /// plans, this separate step is required.
    pub fn forward_refs<E: ProcessEdgesWork>(&self, trace: &mut E, mmtk: &'static MMTK<E::VM>) {
        debug_assert!(
            mmtk.plan.constraints().needs_forward_after_liveness,
            "A plan with needs_forward_after_liveness=false does not need a separate forward step"
        );
        self.soft
            .forward::<E>(trace, mmtk.plan.is_current_gc_nursery());
        self.weak
            .forward::<E>(trace, mmtk.plan.is_current_gc_nursery());
        self.phantom
            .forward::<E>(trace, mmtk.plan.is_current_gc_nursery());
    }

    // Methods for scanning weak references. It needs to be called in a decreasing order of reference strengths, i.e. soft > weak > phantom

    /// Scan soft references.
    pub fn scan_soft_refs<E: ProcessEdgesWork>(&self, trace: &mut E, mmtk: &'static MMTK<E::VM>) {
        // For soft refs, it is up to the VM to decide when to reclaim this.
        // If this is not an emergency collection, we have no heap stress. We simply retain soft refs.
        if !mmtk.plan.is_emergency_collection() {
            // This step only retains the referents (keep the referents alive), it does not update its addresses.
            // We will call soft.scan() again with retain=false to update its addresses based on liveness.
            self.soft
                .retain::<E>(trace, mmtk.plan.is_current_gc_nursery());
        }
        // This will update the references (and the referents).
        self.soft
            .scan::<E>(trace, mmtk.plan.is_current_gc_nursery());
    }

    /// Scan weak references.
    pub fn scan_weak_refs<E: ProcessEdgesWork>(&self, trace: &mut E, mmtk: &'static MMTK<E::VM>) {
        self.weak
            .scan::<E>(trace, mmtk.plan.is_current_gc_nursery());
    }

    /// Scan phantom references.
    pub fn scan_phantom_refs<E: ProcessEdgesWork>(
        &self,
        trace: &mut E,
        mmtk: &'static MMTK<E::VM>,
    ) {
        self.phantom
            .scan::<E>(trace, mmtk.plan.is_current_gc_nursery());
    }
}

impl Default for ReferenceProcessors {
    fn default() -> Self {
        Self::new()
    }
}

// XXX: We differ from the original implementation
//      by ignoring "stress," i.e. where the array
//      of references is grown by 1 each time. We
//      can't do this here b/c std::vec::Vec doesn't
//      allow us to customize its behaviour like that.
//      (Similarly, GROWTH_FACTOR is locked at 2.0, but
//      luckily this is also the value used by Java MMTk.)
const INITIAL_SIZE: usize = 256;

/// We create a reference processor for each semantics. Generally we expect these
/// to happen for each processor:
/// 1. The VM adds reference candidates. They could either do it when a weak reference
///    is created, or when a weak reference is traced during GC.
/// 2. We scan references after the GC determins liveness.
/// 3. We forward references if the GC needs forwarding after liveness.
/// 4. We inform the binding of references whose referents are cleared during this GC by enqueue'ing.
pub struct ReferenceProcessor {
    /// Most of the reference processor is protected by a mutex.
    sync: Mutex<ReferenceProcessorSync>,

    /// The semantics for the reference processor
    semantics: Semantics,

    /// Is it allowed to add candidate to this reference processor? The value is true for most of the time,
    /// but it is set to false once we finish forwarding references, at which point we do not expect to encounter
    /// any 'new' reference in the same GC. This makes sure that no new entry will be added to our reference table once
    /// we finish forwarding, as we will not be able to process the entry in that GC.
    // This avoids an issue in the following scenario in mark compact:
    // 1. First trace: add a candidate WR
    // 2. Weak reference scan: scan the reference table, as MC does not forward object in the first trace. This scan does not update any reference.
    // 3. Second trace: call add_candidate again with WR, but WR gets ignored as we already have WR in our reference table.
    // 4. Weak reference forward: call trace_object for WR, which pushes WR to the node buffer and update WR -> WR' in our reference table.
    // 5. When we trace objects in the node buffer, we will attempt to add WR as a candidate. As we have updated WR to WR' in our reference
    //    table, we would accept WR as a candidate. But we will not trace WR again, and WR will be invalid after this GC.
    // This flag is set to false after Step 4, so in Step 5, we will ignore adding WR.
    allow_new_candidate: AtomicBool,
}

#[derive(Copy, Clone, Debug, PartialEq)]
pub enum Semantics {
    SOFT,
    WEAK,
    PHANTOM,
}

struct ReferenceProcessorSync {
    /// The table of reference objects for the current semantics. We add references to this table by
    /// add_candidate(). After scanning this table, a reference in the table should either
    /// stay in the table (if the referent is alive) or go to enqueued_reference (if the referent is dead and cleared).
    /// Note that this table should not have duplicate entries, otherwise we will scan the duplicates multiple times, and
    /// that may lead to incorrect results.
    references: HashSet<ObjectReference>,

    /// References whose referents are cleared during this GC. We add references to this table during
    /// scanning, and we pop from this table during the enqueue work at the end of GC.
    enqueued_references: Vec<ObjectReference>,

    /// Index into the references table for the start of nursery objects
    nursery_index: usize,
}

impl ReferenceProcessor {
    pub const MAX_REFERENCES_PER_WORK_PACKET: usize = 4096;

    pub fn new(semantics: Semantics) -> Self {
        ReferenceProcessor {
            sync: Mutex::new(ReferenceProcessorSync {
                references: HashSet::with_capacity(INITIAL_SIZE),
                enqueued_references: vec![],
                nursery_index: 0,
            }),
            semantics,
            allow_new_candidate: AtomicBool::new(true),
        }
    }

    /// Add a candidate.
    #[inline(always)]
    pub fn add_candidate(&self, reff: ObjectReference) {
        if !self.allow_new_candidate.load(Ordering::SeqCst) {
            return;
        }

        let mut sync = self.sync.lock().unwrap();
        sync.references.insert(reff);
    }

    pub fn add_candidates(&self, add: &[ObjectReference]) {
        if !self.allow_new_candidate.load(Ordering::SeqCst) {
            return;
        }

        let mut sync = self.sync.lock().unwrap();
        // add
        sync.references.extend(add);
    }

    pub fn add_candidates_and_enqueue<VM: VMBinding>(&self, add: &[ObjectReference], enqueue: &[ObjectReference]) {
        if !self.allow_new_candidate.load(Ordering::SeqCst) {
            return;
        }

        let mut sync = self.sync.lock().unwrap();
        // add
        sync.references.extend(add);
        // enqueue
        sync.enqueued_references.extend(enqueue);
        // debug assert if there are duplicate entries in enqueued_references
        #[cfg(debug_assertions)]
        {
            use std::iter::FromIterator;
            let enqueued_len = sync.enqueued_references.len();
            let enqueued_set: HashSet<ObjectReference> = HashSet::from_iter(sync.enqueued_references.iter().cloned());
            assert_eq!(enqueued_len, enqueued_set.len());
            assert_eq!(enqueued_len, sync.enqueued_references.len());
        }
    }

    pub fn enqueue_candidates(&self, refs: &[ObjectReference]) {
        let mut sync = self.sync.lock().unwrap();
        sync.enqueued_references.extend(refs);

        // debug assert if there are duplicate entries in enqueued_references
        #[cfg(debug_assertions)]
        {
            use std::iter::FromIterator;
            let enqueued_len = sync.enqueued_references.len();
            let enqueued_set: HashSet<ObjectReference> = HashSet::from_iter(sync.enqueued_references.iter().cloned());
            assert_eq!(enqueued_len, enqueued_set.len());
            assert_eq!(enqueued_len, sync.enqueued_references.len());
        }
    }

    fn disallow_new_candidate(&self) {
        self.allow_new_candidate.store(false, Ordering::SeqCst);
    }

    fn allow_new_candidate(&self) {
        self.allow_new_candidate.store(true, Ordering::SeqCst);
    }

    // These funcions simply call `trace_object()`, which does two things: 1. to make sure the object is kept alive
    // and 2. to get the new object reference if the object is copied. The functions are intended to make the code
    // easier to understand.

    #[inline(always)]
    fn get_forwarded_referent<E: ProcessEdgesWork>(
        e: &mut E,
        referent: ObjectReference,
    ) -> ObjectReference {
        e.trace_object(referent)
    }

    #[inline(always)]
    fn get_forwarded_reference<E: ProcessEdgesWork>(
        e: &mut E,
        object: ObjectReference,
    ) -> ObjectReference {
        e.trace_object(object)
    }

    #[inline(always)]
    fn keep_referent_alive<E: ProcessEdgesWork>(
        e: &mut E,
        referent: ObjectReference,
    ) -> ObjectReference {
        e.trace_object(referent)
    }

    pub fn split_ref_table(&self, n_threads: usize, drain: bool) -> Vec<Vec<ObjectReference>> {
        let mut sync = self.sync.lock().unwrap();
        let refs: Vec<ObjectReference> = if drain {
            sync.references.drain().collect()
        } else {
            sync.references.iter().cloned().collect()
        };

        if refs.is_empty() {
            return vec![];
        }

        let refs_per_work_packet = if Self::MAX_REFERENCES_PER_WORK_PACKET * n_threads < refs.len() {
            Self::MAX_REFERENCES_PER_WORK_PACKET
        } else {
            (refs.len() as f32 / n_threads as f32).ceil() as usize
        };
        debug!("{} refs ({} workers): split into {} refs per work", refs.len(), n_threads, refs_per_work_packet);
        refs.chunks(refs_per_work_packet).map(|c| c.into()).collect()
    }

    /// Inform the binding to enqueue the weak references whose referents were cleared in this GC.
    pub fn enqueue<VM: VMBinding>(&self, tls: VMWorkerThread) {
        debug!("enqueue {:?}", self.semantics);
        let mut sync = self.sync.lock().unwrap();

        // This is the end of a GC. We do some assertions here to make sure our reference tables are correct.
        #[cfg(debug_assertions)]
        {
            // For references in the table, the reference needs to be valid, and if the referent is not null, it should be valid as well
            sync.references.iter().for_each(|reff| {
                debug_assert!(!reff.is_null());
                debug_assert!(reff.is_in_any_space());
                let referent = VM::VMReferenceGlue::get_referent(*reff);
                if !referent.is_null() {
                    debug_assert!(
                        referent.is_in_any_space(),
                        "Referent {:?} (of reference {:?}) is not in any space",
                        referent,
                        reff
                    );
                }
            });
            // For references that will be enqueue'd, the referent needs to be valid, and the referent needs to be null.
            sync.enqueued_references.iter().for_each(|reff| {
                debug_assert!(!reff.is_null());
                debug_assert!(reff.is_in_any_space());
                let referent = VM::VMReferenceGlue::get_referent(*reff);
                debug_assert!(referent.is_null());
            });
        }

        if !sync.enqueued_references.is_empty() {
            trace!("enqueue: {:?}", sync.enqueued_references);
            VM::VMReferenceGlue::enqueue_references(&sync.enqueued_references, tls);
            sync.enqueued_references.clear();
        }

        self.allow_new_candidate();
    }

    /// Forward the reference tables in the reference processor. This is only needed if a plan does not forward
    /// objects in their first transitive closure.
    /// nursery is not used for this.
    pub fn forward<E: ProcessEdgesWork>(&self, trace: &mut E, _nursery: bool) {
        let mut sync = self.sync.lock().unwrap();
        debug!("Starting ReferenceProcessor.forward({:?})", self.semantics);

        sync.references = sync
            .references
            .iter()
            .map(|reff| Self::forward_single_reference::<E>(trace, *reff))
            .collect();

        sync.enqueued_references = sync
            .enqueued_references
            .iter()
            .map(|reff| Self::forward_single_reference::<E>(trace, *reff))
            .collect();

        debug!("Ending ReferenceProcessor.forward({:?})", self.semantics);

        // We finish forwarding. No longer accept new candidates.
        self.disallow_new_candidate();
    }

    // Forward a single reference
    #[inline(always)]
    fn forward_single_reference<E: ProcessEdgesWork>(
        trace: &mut E,
        reference: ObjectReference,
    ) -> ObjectReference {
        let old_referent = <E::VM as VMBinding>::VMReferenceGlue::get_referent(reference);
        let new_referent = ReferenceProcessor::get_forwarded_referent(trace, old_referent);
        <E::VM as VMBinding>::VMReferenceGlue::set_referent(reference, new_referent);
        let new_reference = ReferenceProcessor::get_forwarded_reference(trace, reference);
        {
            use crate::vm::ObjectModel;
            trace!(
                "Forwarding reference: {} (size: {})",
                reference,
                <E::VM as VMBinding>::VMObjectModel::get_current_size(reference)
            );
            trace!(
                " referent: {} (forwarded to {})",
                old_referent,
                new_referent
            );
            trace!(" reference: forwarded to {}", new_reference);
        }
        debug_assert!(
            !new_reference.is_null(),
            "reference {:?}'s forwarding pointer is NULL",
            reference
        );
        new_reference
    }

    /// Scan the reference table, and update each reference/referent.
    // TODO: nursery is currently ignored. We used to use Vec for the reference table, and use an int
    // to point to the reference that we last scanned. However, when we use HashSet for reference table,
    // we can no longer do that.
    fn scan<E: ProcessEdgesWork>(&self, trace: &mut E, _nursery: bool) {
        let mut sync = self.sync.lock().unwrap();

        debug!("Starting ReferenceProcessor.scan({:?})", self.semantics);

        trace!(
            "{:?} Reference table is {:?}",
            self.semantics,
            sync.references
        );

        debug_assert!(sync.enqueued_references.is_empty());
        // Put enqueued reference in this vec
        let mut enqueued_references = vec![];

        // Determinine liveness for each reference and only keep the refs if `process_reference()` returns Some.
        let new_set: HashSet<ObjectReference> = sync
            .references
            .iter()
            .filter_map(|reff| Self::scan_single_reference(trace, *reff, &mut enqueued_references))
            .collect();

        debug!(
            "{:?} reference table from {} to {} ({} enqueued)",
            self.semantics,
            sync.references.len(),
            new_set.len(),
            enqueued_references.len()
        );
        sync.references = new_set;
        sync.enqueued_references = enqueued_references;

        debug!("Ending ReferenceProcessor.scan({:?})", self.semantics);
    }

    /// Retain referent in the reference table. This method deals only with soft references.
    /// It retains the referent if the reference is definitely reachable. This method does
    /// not update reference or referent. So after this method, scan() should be used to update
    /// the references/referents.
    fn retain<E: ProcessEdgesWork>(&self, trace: &mut E, _nursery: bool) {
        debug_assert!(self.semantics == Semantics::SOFT);

        let sync = self.sync.lock().unwrap();

        debug!("Starting ReferenceProcessor.retain({:?})", self.semantics);
        trace!(
            "{:?} Reference table is {:?}",
            self.semantics,
            sync.references
        );

        for reference in sync.references.iter() {
            Self::retain_single_reference(trace, *reference);
        }

        debug!("Ending ReferenceProcessor.retain({:?})", self.semantics);
    }

    #[inline(always)]
    pub fn retain_single_reference<E: ProcessEdgesWork>(trace: &mut E, reference: ObjectReference) {
        debug_assert!(!reference.is_null());

        trace!("Processing reference: {:?}", reference);

        if !reference.is_live() {
            // Reference is currently unreachable but may get reachable by the
            // following trace. We postpone the decision.
            return;
        }

        // Reference is definitely reachable.  Retain the referent.
        let referent = <E::VM as VMBinding>::VMReferenceGlue::get_referent(reference);
        if !referent.is_null() {
            Self::keep_referent_alive(trace, referent);
        }
        trace!(" ~> {:?} (retained)", referent.to_address());
    }

    /// Process a reference.
    /// * If both the reference and the referent is alive, return the updated reference and update its referent properly.
    /// * If the reference is alive, and the referent is not null but not alive, return None and the reference (with cleared referent) is enqueued.
    /// * For other cases, return None.
    ///
    /// If a None value is returned, the reference can be removed from the reference table. Otherwise, the updated reference should be kept
    /// in the reference table.
    pub fn scan_single_reference<E: ProcessEdgesWork>(
        trace: &mut E,
        reference: ObjectReference,
        enqueued_references: &mut Vec<ObjectReference>,
    ) -> Option<ObjectReference> {
        debug_assert!(!reference.is_null());

        trace!("Process reference: {}", reference);

        // If the reference is dead, we're done with it. Let it (and
        // possibly its referent) be garbage-collected.
        if !reference.is_live() {
            <E::VM as VMBinding>::VMReferenceGlue::clear_referent(reference);
            trace!(" UNREACHABLE reference: {}", reference);
            trace!(" (unreachable)");
            return None;
        }

        // The reference object is live
        let new_reference = Self::get_forwarded_reference(trace, reference);
        let old_referent = <E::VM as VMBinding>::VMReferenceGlue::get_referent(reference);
        trace!(" ~> {}", old_referent);

        // If the application has cleared the referent the Java spec says
        // this does not cause the Reference object to be enqueued. We
        // simply allow the Reference object to fall out of our
        // waiting list.
        if old_referent.is_null() {
            trace!(" (null referent) ");
            return None;
        }

        trace!(" => {}", new_reference);

        if old_referent.is_live() {
            // Referent is still reachable in a way that is as strong as
            // or stronger than the current reference level.
            let new_referent = Self::get_forwarded_referent(trace, old_referent);
            debug_assert!(new_referent.is_live());
            trace!(" ~> {}", new_referent);

            // The reference object stays on the waiting list, and the
            // referent is untouched. The only thing we must do is
            // ensure that the former addresses are updated with the
            // new forwarding addresses in case the collector is a
            // copying collector.

            // Update the referent
            <E::VM as VMBinding>::VMReferenceGlue::set_referent(new_reference, new_referent);
            Some(new_reference)
        } else {
            // Referent is unreachable. Clear the referent and enqueue the reference object.
            trace!(" UNREACHABLE referent: {}", old_referent);

            <E::VM as VMBinding>::VMReferenceGlue::clear_referent(new_reference);
            enqueued_references.push(new_reference);
            None
        }
    }
}

use crate::scheduler::GCWork;
use crate::scheduler::GCWorker;
use crate::MMTK;
use std::marker::PhantomData;

#[derive(Default)]
pub struct SoftRefProcessing<E: ProcessEdgesWork>(PhantomData<E>);
impl<E: ProcessEdgesWork> GCWork<E::VM> for SoftRefProcessing<E> {
    fn do_work(&mut self, worker: &mut GCWorker<E::VM>, mmtk: &'static MMTK<E::VM>) {
        let mut w = E::new(vec![], false, mmtk);
        w.set_worker(worker);
        mmtk.reference_processors.scan_soft_refs(&mut w, mmtk);
        w.flush();
    }
}
impl<E: ProcessEdgesWork> SoftRefProcessing<E> {
    pub fn new() -> Self {
        Self(PhantomData)
    }
}

#[derive(Default)]
pub struct WeakRefProcessing<E: ProcessEdgesWork>(PhantomData<E>);
impl<E: ProcessEdgesWork> GCWork<E::VM> for WeakRefProcessing<E> {
    fn do_work(&mut self, worker: &mut GCWorker<E::VM>, mmtk: &'static MMTK<E::VM>) {
        let mut w = E::new(vec![], false, mmtk);
        w.set_worker(worker);
        mmtk.reference_processors.scan_weak_refs(&mut w, mmtk);
        w.flush();
    }
}
impl<E: ProcessEdgesWork> WeakRefProcessing<E> {
    pub fn new() -> Self {
        Self(PhantomData)
    }
}

#[derive(Default)]
pub struct PhantomRefProcessing<E: ProcessEdgesWork>(PhantomData<E>);
impl<E: ProcessEdgesWork> GCWork<E::VM> for PhantomRefProcessing<E> {
    fn do_work(&mut self, worker: &mut GCWorker<E::VM>, mmtk: &'static MMTK<E::VM>) {
        let mut w = E::new(vec![], false, mmtk);
        w.set_worker(worker);
        mmtk.reference_processors.scan_phantom_refs(&mut w, mmtk);
        w.flush();
    }
}
impl<E: ProcessEdgesWork> PhantomRefProcessing<E> {
    pub fn new() -> Self {
        Self(PhantomData)
    }
}

#[derive(Default)]
pub struct RefForwarding<E: ProcessEdgesWork>(PhantomData<E>);
impl<E: ProcessEdgesWork> GCWork<E::VM> for RefForwarding<E> {
    fn do_work(&mut self, worker: &mut GCWorker<E::VM>, mmtk: &'static MMTK<E::VM>) {
        let mut w = E::new(vec![], false, mmtk);
        w.set_worker(worker);
        mmtk.reference_processors.forward_refs(&mut w, mmtk);
        w.flush();
    }
}
impl<E: ProcessEdgesWork> RefForwarding<E> {
    pub fn new() -> Self {
        Self(PhantomData)
    }
}

#[derive(Default)]
pub struct RefEnqueue<VM: VMBinding>(PhantomData<VM>);
impl<VM: VMBinding> GCWork<VM> for RefEnqueue<VM> {
    fn do_work(&mut self, worker: &mut GCWorker<VM>, mmtk: &'static MMTK<VM>) {
        mmtk.reference_processors.enqueue_refs::<VM>(worker.tls);
    }
}
impl<VM: VMBinding> RefEnqueue<VM> {
    pub fn new() -> Self {
        Self(PhantomData)
    }
}

use std::sync::Arc;

pub struct ReferenceBuffer {
    pub do_buffer: AtomicBool,
    reference_processors: Arc<ReferenceProcessors>,
    pub soft_refs: Vec<ObjectReference>,
    pub weak_refs: Vec<ObjectReference>,
    pub phantom_refs: Vec<ObjectReference>,
}

impl ReferenceBuffer {
    pub fn new(reference_processors: Arc<ReferenceProcessors>) -> Self {
        Self {
            do_buffer: AtomicBool::new(false),
            reference_processors,
            soft_refs: vec![],
            weak_refs: vec![],
            phantom_refs: vec![],
        }
    }

    #[inline(always)]
    pub fn add_soft_ref(&mut self, reff: ObjectReference) {
        if self.do_buffer.load(Ordering::Relaxed) {
            self.soft_refs.push(reff);
        } else {
            self.reference_processors.get(Semantics::SOFT).add_candidate(reff);
        }
    }

    #[inline(always)]
    pub fn add_weak_ref(&mut self, reff: ObjectReference) {
        if self.do_buffer.load(Ordering::Relaxed) {
            self.weak_refs.push(reff);
        } else {
            self.reference_processors.get(Semantics::WEAK).add_candidate(reff);
        }
    }

    #[inline(always)]
    pub fn add_phantom_ref(&mut self, reff: ObjectReference) {
        if self.do_buffer.load(Ordering::Relaxed) {
            self.phantom_refs.push(reff);
        } else {
            self.reference_processors.get(Semantics::PHANTOM).add_candidate(reff);
        }
    }

    pub fn flush(&mut self) {
        assert!(self.soft_refs.is_empty());
        assert!(self.weak_refs.is_empty());
        assert!(self.phantom_refs.is_empty());
        // self.do_buffer.store(false, Ordering::SeqCst);

        // if !self.soft_refs.is_empty() {
        //     self.reference_processors.add_soft_candidates(&self.soft_refs);
        //     self.soft_refs.clear();
        // }

        // if !self.weak_refs.is_empty() {
        //     self.reference_processors.add_weak_candidates(&self.weak_refs);
        //     self.weak_refs.clear();
        // }

        // if !self.phantom_refs.is_empty() {
        //     self.reference_processors.add_phantom_candidates(&self.phantom_refs);
        //     self.phantom_refs.clear();
        // }
    }

    // pub fn distribute_work<E: ProcessEdgesWork>(&mut self, mmtk: &MMTK<E::VM>) {
    //     self.do_buffer.store(false, Ordering::SeqCst);

    //     if !self.soft_refs.is_empty() {
    //         // retain & scan soft
    //         let mut ref_chunks: Vec<Vec<ObjectReference>> = self.soft_refs.chunks(ReferenceProcessor::MAX_REFERENCES_PER_WORK_PACKET).map(|c| c.into()).collect();
    //         let packets = ref_chunks.drain(..).map(|refs| Box::new(MTRetainAndScanRefs::<E> { refs, semantic: Semantics::SOFT, retain: !mmtk.plan.is_emergency_collection(), _p: PhantomData }) as Box<dyn GCWork<E::VM>>).collect();
    //         mmtk.scheduler.work_buckets[crate::scheduler::WorkBucketStage::SoftRefClosure].bulk_add(packets);

    //         self.soft_refs.clear();
    //     }

    //     if !self.weak_refs.is_empty() {
    //         // scan weak
    //         let mut ref_chunks: Vec<Vec<ObjectReference>> = self.weak_refs.chunks(ReferenceProcessor::MAX_REFERENCES_PER_WORK_PACKET).map(|c| c.into()).collect();
    //         let packets = ref_chunks.drain(..).map(|refs| Box::new(MTRetainAndScanRefs::<E> { refs, semantic: Semantics::WEAK, retain: false, _p: PhantomData }) as Box<dyn GCWork<E::VM>>).collect();
    //         mmtk.scheduler.work_buckets[crate::scheduler::WorkBucketStage::WeakRefClosure].bulk_add(packets);

    //         self.weak_refs.clear();
    //     }

    //     if !self.phantom_refs.is_empty() {
    //         // scan phantom
    //         let mut ref_chunks: Vec<Vec<ObjectReference>> = self.phantom_refs.chunks(ReferenceProcessor::MAX_REFERENCES_PER_WORK_PACKET).map(|c| c.into()).collect();
    //         let packets = ref_chunks.drain(..).map(|refs| Box::new(MTRetainAndScanRefs::<E> { refs, semantic: Semantics::PHANTOM, retain: false, _p: PhantomData }) as Box<dyn GCWork<E::VM>>).collect();
    //         mmtk.scheduler.work_buckets[crate::scheduler::WorkBucketStage::PhantomRefClosure].bulk_add(packets);

    //         self.phantom_refs.clear();
    //     }
    // }
}

// Multi threaded reference processing

// #[derive(Default)]
// pub struct MTPrepareRefs<E: ProcessEdgesWork>(pub PhantomData<E>);
// impl<E: ProcessEdgesWork> GCWork<E::VM> for MTPrepareRefs<E> {
//     fn do_work(&mut self, worker: &mut GCWorker<E::VM>, mmtk: &'static MMTK<E::VM>) {
//         {
//             let ref ref_processor = mmtk.reference_processors.soft;
//             // Split work
//             let mut ref_chunks: Vec<Vec<ObjectReference>> = ref_processor.split_ref_table(*mmtk.get_options().threads, true);
//             let packets = ref_chunks.drain(..).map(|refs| Box::new(MTRetainAndScanRefs::<E> { refs, semantic: Semantics::SOFT, retain: !mmtk.plan.is_emergency_collection(), _p: PhantomData }) as Box<dyn GCWork<E::VM>>).collect();
//             mmtk.scheduler.work_buckets[crate::scheduler::WorkBucketStage::SoftRefClosure].bulk_add(packets);
//         }

//         {
//             let ref ref_processor = mmtk.reference_processors.weak;
//             // Split work
//             let mut ref_chunks: Vec<Vec<ObjectReference>> = ref_processor.split_ref_table(*mmtk.get_options().threads, true);
//             let packets = ref_chunks.drain(..).map(|refs| Box::new(MTRetainAndScanRefs::<E> { refs, semantic: Semantics::WEAK, retain: false, _p: PhantomData }) as Box<dyn GCWork<E::VM>>).collect();
//             mmtk.scheduler.work_buckets[crate::scheduler::WorkBucketStage::WeakRefClosure].bulk_add(packets);
//         }

//         {
//             let ref ref_processor = mmtk.reference_processors.phantom;
//             // Split work
//             let mut ref_chunks: Vec<Vec<ObjectReference>> = ref_processor.split_ref_table(*mmtk.get_options().threads, true);
//             let packets = ref_chunks.drain(..).map(|refs| Box::new(MTRetainAndScanRefs::<E> { refs, semantic: Semantics::PHANTOM, retain: false, _p: PhantomData }) as Box<dyn GCWork<E::VM>>).collect();
//             mmtk.scheduler.work_buckets[crate::scheduler::WorkBucketStage::PhantomRefClosure].bulk_add(packets);
//         }
//     }
// }

// impl<E: ProcessEdgesWork> MTPrepareRefs<E> {
//     pub fn new() -> Self {
//         Self(PhantomData)
//     }
// }

// pub struct MTRetainRefs<E: ProcessEdgesWork> {
//     refs: Vec<ObjectReference>,
//     _p: PhantomData<E>,
// }
// impl<E: ProcessEdgesWork> GCWork<E::VM> for MTRetainRefs<E> {
//     fn do_work(&mut self, worker: &mut GCWorker<E::VM>, mmtk: &'static MMTK<E::VM>) {
//         let mut w = E::new(vec![], false, mmtk);
//         w.set_worker(worker);
//         self.refs.iter().for_each(|r| ReferenceProcessor::retain_single_reference(&mut w, *r));
//         w.flush();
//     }
// }

// pub struct MTRetainAndScanRefs<E: ProcessEdgesWork> {
//     refs: Vec<ObjectReference>,
//     semantic: Semantics,
//     retain: bool,
//     _p: PhantomData<E>,
// }
// impl<E: ProcessEdgesWork> GCWork<E::VM> for MTRetainAndScanRefs<E> {
//     fn do_work(&mut self, worker: &mut GCWorker<E::VM>, mmtk: &'static MMTK<E::VM>) {
//         debug!("MTRetainAndScanRefs {:?} (retain: {}): {:?} refs", self.semantic, self.retain, self.refs.len());
//         let mut w = E::new(vec![], false, mmtk);
//         w.set_worker(worker);
//         if self.retain {
//             self.refs.iter().for_each(|r| ReferenceProcessor::retain_single_reference(&mut w, *r));
//             w.flush();
//         }

//         let mut enqueue = vec![];
//         let updated_refs: Vec<ObjectReference> = self.refs.iter().filter_map(|r| ReferenceProcessor::scan_single_reference(&mut w, *r, &mut enqueue)).collect();
//         mmtk.reference_processors.get(self.semantic).add_candidates_and_enqueue::<E::VM>(&updated_refs, &enqueue);
//         w.flush();
//     }
// }

// #[derive(Default)]
// pub struct MTPrepareScanRefs<E: ProcessEdgesWork>(PhantomData<E>);
// impl<E: ProcessEdgesWork> GCWork<E::VM> for MTPrepareScanRefs<E> {
//     fn do_work(&mut self, worker: &mut GCWorker<E::VM>, mmtk: &'static MMTK<E::VM>) {
//         // scan soft refs
//         let ref soft_ref_processor = mmtk.reference_processors.soft;
//         // Split work
//         let mut ref_chunks: Vec<Vec<ObjectReference>> = soft_ref_processor.split_ref_table(*mmtk.get_options().threads, true);
//         let packets = ref_chunks.drain(..).map(|refs| Box::new(MTScanRefs::<E> { refs, semantic: Semantics::SOFT, _p: PhantomData }) as Box<dyn GCWork<E::VM>>).collect();
//         mmtk.scheduler.work_buckets[crate::scheduler::WorkBucketStage::WeakRefClosure].bulk_add(packets);

//         // scan weak refs
//         let ref weak_ref_processor = mmtk.reference_processors.weak;
//         // Split work
//         let mut ref_chunks: Vec<Vec<ObjectReference>> = weak_ref_processor.split_ref_table(*mmtk.get_options().threads, true);
//         let packets = ref_chunks.drain(..).map(|refs| Box::new(MTScanRefs::<E> { refs, semantic: Semantics::WEAK, _p: PhantomData }) as Box<dyn GCWork<E::VM>>).collect();
//         mmtk.scheduler.work_buckets[crate::scheduler::WorkBucketStage::WeakRefClosure].bulk_add(packets);

//         // scan phantom refs
//         let ref phantom_ref_processor = mmtk.reference_processors.phantom;
//         // Split work
//         let mut ref_chunks: Vec<Vec<ObjectReference>> = phantom_ref_processor.split_ref_table(*mmtk.get_options().threads, true);
//         let packets = ref_chunks.drain(..).map(|refs| Box::new(MTScanRefs::<E> { refs, semantic: Semantics::PHANTOM, _p: PhantomData }) as Box<dyn GCWork<E::VM>>).collect();
//         mmtk.scheduler.work_buckets[crate::scheduler::WorkBucketStage::PhantomRefClosure].bulk_add(packets);
//     }
// }
// impl<E: ProcessEdgesWork> MTPrepareScanRefs<E> {
//     pub fn new() -> Self {
//         Self(PhantomData)
//     }
// }

// pub struct MTScanRefs<E: ProcessEdgesWork> {
//     refs: Vec<ObjectReference>,
//     semantic: Semantics,
//     _p: PhantomData<E>,
// }
// impl<E: ProcessEdgesWork> GCWork<E::VM> for MTScanRefs<E> {
//     fn do_work(&mut self, worker: &mut GCWorker<E::VM>, mmtk: &'static MMTK<E::VM>) {
//         debug!("MTScanRefs {:?}: {:?} refs", self.semantic, self.refs.len());
//         let mut w = E::new(vec![], false, mmtk);
//         let mut enqueue = vec![];
//         w.set_worker(worker);
//         let updated_refs: Vec<ObjectReference> = self.refs.iter().filter_map(|r| ReferenceProcessor::scan_single_reference(&mut w, *r, &mut enqueue)).collect();
//         mmtk.reference_processors.get(self.semantic).add_candidates_and_enqueue::<E::VM>(&updated_refs, &enqueue);
//         w.flush();
//     }
// }

#[derive(Default)]
pub struct ScheduleFlushWorkerRefBuffer<E: ProcessEdgesWork>(pub PhantomData<E>);
impl<E: ProcessEdgesWork> GCWork<E::VM> for ScheduleFlushWorkerRefBuffer<E> {
    fn do_work(&mut self, _worker: &mut GCWorker<E::VM>, mmtk: &'static MMTK<E::VM>) {
        for w in mmtk.scheduler.worker_group.workers_shared.iter() {
            let work_added = w.designated_work.push(Box::new(FlushWorkerRefBuffer::<E>(PhantomData)));
            assert!(work_added.is_ok());
        }
        // let _guard = mmtk.scheduler.worker_monitor.0.lock().unwrap();
        // mmtk.scheduler.worker_monitor.1.notify_all();
    }
}

// Flush worker ref buffer
#[derive(Default)]
pub struct FlushWorkerRefBuffer<E: ProcessEdgesWork>(PhantomData<E>);
impl<E: ProcessEdgesWork> GCWork<E::VM> for FlushWorkerRefBuffer<E> {
    fn do_work(&mut self, worker: &mut GCWorker<E::VM>, mmtk: &'static MMTK<E::VM>) {
        assert!(!mmtk.scheduler.work_buckets[crate::scheduler::WorkBucketStage::SoftRefClosure].is_activated(), "RefClosure bucket was opened before we flush worker ref buffer");
        worker.reference_buffer.flush();
    }
}
