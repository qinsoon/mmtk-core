//! This module holds work packets for `CommonPlan` and `BasePlan`, or other work packets not
//! directly related to scheduling.

use crate::{plan::global::CommonPlan, policy::space::Space, scheduler::GCWork, vm::VMBinding};

pub(super) struct SetCommonPlanUnlogBits<VM: VMBinding> {
    pub common_plan: &'static CommonPlan<VM>,
    /// A plan's own LOS, if any. Not part of `CommonPlan` since its concrete type can differ per
    /// plan, so it's threaded through separately here.
    pub extra_space: Option<&'static dyn Space<VM>>,
}

impl<VM: VMBinding> GCWork<VM> for SetCommonPlanUnlogBits<VM> {
    fn do_work(
        &mut self,
        _worker: &mut crate::scheduler::GCWorker<VM>,
        _mmtk: &'static crate::MMTK<VM>,
    ) {
        self.common_plan.set_side_log_bits();
        if let Some(space) = self.extra_space {
            space.set_side_log_bits();
        }
    }
}

pub(super) struct ClearCommonPlanUnlogBits<VM: VMBinding> {
    pub common_plan: &'static CommonPlan<VM>,
    /// A plan's own LOS, if any. Not part of `CommonPlan` since its concrete type can differ per
    /// plan, so it's threaded through separately here.
    pub extra_space: Option<&'static dyn Space<VM>>,
}

impl<VM: VMBinding> GCWork<VM> for ClearCommonPlanUnlogBits<VM> {
    fn do_work(
        &mut self,
        _worker: &mut crate::scheduler::GCWorker<VM>,
        _mmtk: &'static crate::MMTK<VM>,
    ) {
        self.common_plan.clear_side_log_bits();
        if let Some(space) = self.extra_space {
            space.clear_side_log_bits();
        }
    }
}
