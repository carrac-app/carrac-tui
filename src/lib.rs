/// Thin library shim so `tests/` can import crate types without duplicating them.
///
/// Only the types modules are exposed — the binary entry point remains in main.rs.
pub mod types;
