use crate::policy::space::Space;
use crate::policy::sft::SFT;
use crate::vm::VMBinding;
use crate::policy::space::PlanCreateSpaceArgs;
use crate::policy::gc_work::PolicyTraceObject;

pub trait VMSpace<VM: VMBinding>: Space<VM> + SFT + PolicyTraceObject<VM> {
    fn new(args: PlanCreateSpaceArgs<VM>) -> Self;
    fn prepare(&mut self);
    fn release(&mut self);
}

use crate::policy::immortalspace::ImmortalSpace;

impl<VM: VMBinding> VMSpace<VM> for ImmortalSpace<VM> {
    fn new(args: PlanCreateSpaceArgs<VM>) -> Self {
        ImmortalSpace::new(args)
    }

    fn prepare(&mut self) {
        ImmortalSpace::<VM>::prepare(self)
    }

    fn release(&mut self) {
        ImmortalSpace::<VM>::release(self)
    }
}
