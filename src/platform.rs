//! Compatibility facade for the machine-level product contract.
//!
//! New reusable code belongs in `wbox-machine`; the binary keeps this module so
//! existing CLI and backend call sites do not need to know crate layout.

pub use wbox_machine::*;
