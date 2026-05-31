#![allow(unused, dead_code, non_upper_case_globals, non_camel_case_types, unused_assignments, unused_mut)]

//! Chaos Monolith Kernel — refactored modular architecture.
//!
//! This crate is a modular rewrite of the original monolithic `kernel.rs`.
//! Each subsystem is organized into its own module for improved readability
//! and maintainability.

pub mod consts;
pub mod sync;
pub mod signal;
pub mod timer;
pub mod memory;
pub mod util;
pub mod channel;
pub mod fs;
pub mod ipc;
pub mod trap;
pub mod process;
pub mod sched;
pub mod kernel;

// Re-export all public items so tests can use `use kernel_refactored::*`
pub use consts::*;
pub use sync::*;
pub use signal::*;
pub use timer::*;
pub use memory::*;
pub use util::*;
pub use channel::*;
pub use fs::*;
pub use ipc::*;
pub use trap::*;
pub use process::*;
pub use sched::*;
pub use kernel::*;
