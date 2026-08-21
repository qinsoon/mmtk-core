//! Ref-counting (LXR) flavors of the generic tracing space policies.
//!
//! Each type here wraps the corresponding generic, tracing-only policy (e.g. [`LXRSpace`]
//! wraps [`crate::policy::immix::ImmixSpace`]) and layers reference-counting semantics on top,
//! reusing the inner space's block/chunk/page-resource/treadmill machinery rather than
//! duplicating it.

pub mod block;
pub mod immixspace;
pub mod largeobjectspace;

pub use block::LXRBlockExt;
pub use immixspace::{ImmixHooks, LXRSpace};
pub use largeobjectspace::LXRLargeObjectSpace;
