pub mod handler;

#[cfg(feature = "s3")]
pub mod s3_handler;

#[cfg(feature = "local_fs")]
pub mod local_fs_handler;

#[cfg(feature = "in_memory")]
pub mod in_memory_handler;

pub mod core;
