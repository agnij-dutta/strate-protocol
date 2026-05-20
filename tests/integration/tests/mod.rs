//! Integration test crate entry. Per Cargo's autodiscovery, each
//! `tests/*.rs` is a separate test binary; this file is reserved for
//! cross-binary shared helpers if any are needed in future.
//!
//! Right now, all shared helpers live in `strate_integration_tests`
//! (the rlib at `src/lib.rs`). Test files `use
//! strate_integration_tests::Fixture;` and so on.
