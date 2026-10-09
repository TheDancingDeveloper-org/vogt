//! Use-cases. Ports `src/vogt/application/`.
//!
//! `context`, `writes` and `resolve` are the seams every later service uses.
//! `instance` is the bootstrap use-case.

pub mod brief;
pub mod context;
pub mod contracts;
pub mod coverage;
pub mod instance;
pub mod observations;
pub mod projects;
pub mod resolve;
pub mod services;
pub mod suppressions;
pub mod views;
pub mod writes;
#[cfg(test)]
mod writes_test;
