pub mod handler;

#[cfg(feature = "s3")]
pub mod s3_handler;

#[cfg(feature = "local_fs")]
pub mod local_fs_handler;

// Not reachable outside this crate's own test builds (#39): an in-memory
// backend with no bound on entry count or byte size is a real, unbounded
// RSS-growth outage risk if it were ever selectable in production. See the
// doc comment on `InMemoryStorage` for the full rationale.
#[cfg(all(test, feature = "in_memory"))]
pub mod in_memory_handler;

pub mod core;

pub mod key_validation;
