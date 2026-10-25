//! Motor Rust de Capi.
//!
//! Sustituye al daemon Kotlin `capi-core`. No depende de Tauri y puede
//! probarse de forma aislada con `cargo test` y `examples/probe.rs`.

pub mod engine;
pub mod error;
pub mod lyrics;
pub mod models;

pub use engine::Engine;
pub use error::EngineError;
pub use models::*;

pub const VERSION: &str = env!("CARGO_PKG_VERSION");
