//! Port 443 SNI pass-through: peek the ClientHello, resolve the SNI host, splice bytes.
//! No decryption, no caching.
//!
//! See TASK-27.
