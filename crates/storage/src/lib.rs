//! Durable local library storage.

pub mod library;
pub mod operation_journal;
mod schema;

pub use library::Catalog;
