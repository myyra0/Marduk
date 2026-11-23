use std::path::Path;

use crate::{GateError, WriterGate, WriterGuard};

/// Held instance gate. Dropping it (or exiting the process) releases the lock.
pub struct InstanceGuard {
    _guard: WriterGuard,
}

/// Take the catalog's instance gate without waiting.
pub fn acquire_instance_guard(catalog: &Path) -> Result<InstanceGuard, GateError> {
    let root = crate::open_private_directory(catalog).map_err(|_| GateError::Unavailable)?;
    let gate = WriterGate::open_at(&root, "instance.lock")?;
    gate.try_lock().map(|guard| InstanceGuard { _guard: guard })
}
