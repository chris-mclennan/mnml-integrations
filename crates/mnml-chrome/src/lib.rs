//! Shared chrome for the mnml integration apps.
//!
//! Mirrors the design-system modules in mnml core (`src/ui/*`), which
//! are not importable here — core is a binary crate, and these apps
//! depend on `mnml-bridge` from crates.io rather than on core itself.
//! Anything in this crate MUST match its core counterpart; the module
//! docs name the file to check against.
//!
//! Added 2026-09-01 after the refresh affordance drifted three ways
//! across the family: core drew a codicon, Jira drew `⟳` (U+27F3), and
//! Bitbucket drew `\u{f0450}` — three different icons for one action,
//! in one product.

pub mod refresh;
