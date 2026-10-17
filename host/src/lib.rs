//! `wp`: pipelines of WebAssembly components connected by typed WASI 0.3
//! streams.
//!
//! ```text
//! cat access.log | parse-log | errors | top-paths 10
//! ```
//!
//! Every stage after the first gets the previous stage's stream as its first
//! argument. The host checks that each pipe's element types line up before
//! running anything, then carries rows between components with a [`pump`]
//! that is much cheaper than Wasmtime's generic copy.

mod builtins;
pub mod pump;
pub mod run;
pub mod stats;

pub use run::{Copy, Options, PipeReport, Report, Runner};
