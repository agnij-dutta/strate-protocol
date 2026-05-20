//! # `strate-interfaces`
//!
//! Shared types for every Strate contract:
//!
//! - [`errors`] — the canonical `Error` enum (Soroban `contracterror`)
//!   used uniformly across the six contracts. One enum, one mapping,
//!   so frontend / indexer code can match on a single namespace.
//! - [`events`] — typed event structs with their topic schemas. Each
//!   event publishes a `(topic_tag, key1, key2?, ...)` topic tuple
//!   plus a `data` struct.
//! - [`traits`] — pure-signature traits each contract exposes. No
//!   implementations; the concrete contracts in `contracts/*` provide
//!   them. The traits give a single audit surface for the public ABI
//!   and let test code mock individual contracts.
//!
//! This crate intentionally has *no* protocol logic. If you find
//! yourself wanting to add a default impl here, it belongs in
//! `strate-math` or the contract itself.

#![no_std]
#![deny(unsafe_code)]
#![warn(clippy::pedantic)]
#![allow(clippy::module_name_repetitions)]
#![allow(clippy::missing_errors_doc)]
#![allow(clippy::missing_panics_doc)]
#![allow(clippy::must_use_candidate)]

pub mod errors;
pub mod events;
pub mod traits;

pub use errors::Error;
