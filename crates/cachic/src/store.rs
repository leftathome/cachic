//! The hybrid RAM + disk store: a foyer wrapper, the self-describing slice codec, and
//! the rebuildable redb object index.
//!
//! The index is never authoritative. See TASK-11, ADR 0003 and ADR 0004.
