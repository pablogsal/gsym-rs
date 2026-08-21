//! Deployment and operations guides.
//!
//! - [`deployment`]: cache roots, epochs, trust boundaries, and filesystem
//!   requirements.
#![cfg_attr(
    feature = "manage",
    doc = " - [`operations`]: population, access tracking, pruning, and crash recovery."
)]
#![cfg_attr(
    feature = "manage",
    doc = " - [`cookbook`]: complete recipes built from the public API."
)]

/// Cache roots, epochs, trust boundaries, and filesystem requirements.
#[doc = include_str!("../docs/deployment.md")]
pub mod deployment {}

/// Population, access tracking, pruning, and crash recovery.
#[cfg(feature = "manage")]
#[doc = include_str!("../docs/operations.md")]
pub mod operations {}

/// Complete cache API recipes.
#[cfg(feature = "manage")]
#[doc = include_str!("../docs/cookbook.md")]
pub mod cookbook {}
