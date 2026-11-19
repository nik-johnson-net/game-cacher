//! Caching engine (lancache-style sliced HTTP cache). See docs/ARCHITECTURE.md
//! "Cache engine semantics".

pub mod engine;
pub mod index;
pub mod key;
pub mod meta;
mod proxy;
pub mod range;

pub use engine::{CacheEngine, CacheError, EngineDeps};
pub use index::{LruIndex, Usage};
pub use proxy::STATUS_HEADER;
