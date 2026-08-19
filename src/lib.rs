//! Library root for the `emgr` crate.
//!
//! This exists so that criterion benches (`benches/*.rs`) and integration
//! tests (`tests/*.rs`) can link against the pipeline internals
//! (`ImageService`, `CacheService`, `ResizeQuery`, ...) as an ordinary
//! external crate (`emgr::...`), instead of duplicating that logic.
//!
//! Deliberately **not** exported here: `modules` (in particular
//! `modules::api`, owned by other agents and out of scope for this change).
//! `src/main.rs` keeps its own local `mod config; mod models; mod modules;
//! mod services;` tree exactly as before - unrelated to this lib target -
//! so the binary crate is untouched. Keeping `modules` out of this lib
//! means the benches/tests introduced alongside this file do not need to
//! compile `modules::api` at all, and are therefore unaffected by it.
pub mod config;
pub mod models;
pub mod services;

/// Only the `env` submodule of `modules` - `config::performance` needs
/// `EnvConfig` for its `From<&EnvConfig>` impl. Resolves to the same
/// `src/modules/env/` files `main.rs`'s own local module tree uses; nothing
/// under `modules::api` is reachable through this path.
pub mod modules {
    pub mod env;

    /// Only the leaf helpers `config::performance` needs. `cgroup` has no
    /// dependencies of its own, so exposing it here does not drag the
    /// axum-dependent parts of `modules::utils` (`err`, `etag`) into the
    /// lib target.
    pub mod utils {
        pub mod cgroup;
    }
}
