//! The engine's typed upgrade of a family across its manifest edge.

/// Deterministic interruption points inside an upgrade's durable write.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UpgradeFault {
    /// The converted rows are written; the log and format marker are not yet.
    AfterRowsWritten,
}

pub trait MemoryUpgrades {}
