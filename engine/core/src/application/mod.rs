//! Use-cases. Ports `src/vogt/application/`.
//!
//! `context`, `writes` and `resolve` are the seams every later service uses.
//! `instance` is the bootstrap use-case.

pub mod context;
pub mod instance;
pub mod resolve;
pub mod writes;
#[cfg(test)]
mod writes_test;
