//! Caching engine (lancache-style sliced HTTP cache). See docs/ARCHITECTURE.md
//! "Cache engine semantics".

pub mod engine;
pub mod index;
pub mod key;

pub use engine::CacheEngine;
pub use index::{LruIndex, Usage};
