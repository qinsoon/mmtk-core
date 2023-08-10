// NOTE: Since the dummyvm uses a global MMTK instance,
// it will panic if MMTK initialized more than once per process.
// We run each of the following modules in a separate test process.
//
// One way to avoid re-initialization is to have only one #[test] per module.
// There are also helpers for creating fixtures in `fixture/mod.rs`.
mod issue139_allocate_unaligned_object_size;
mod issue867_allocate_unrealistically_large_object;
mod handle_mmap_oom;
#[cfg(target_os = "linux")]
mod handle_mmap_conflict;
mod allocate_align_offset;
mod allocate_without_initialize_collection;
mod allocate_with_initialize_collection;
mod allocate_with_disable_collection;
mod allocate_with_re_enable_collection;
#[cfg(not(feature = "malloc_counted_size"))]
mod malloc_api;
#[cfg(feature = "malloc_counted_size")]
mod malloc_counted;
mod malloc_ms;
#[cfg(feature = "is_mmtk_object")]
mod conservatism;
mod is_in_mmtk_spaces;
mod fixtures;
mod edges_test;
mod barrier_slow_path_assertion;
