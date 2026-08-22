//! Language bindings for [`auru_pm_kernel`].
//!
//! One surface, two bindings. [`surface`] defines every operation once in plain
//! Rust — bytes in, JSON out — and the `wasm` and `node` modules are mechanical
//! wrappers over it. That is deliberate: a binding that reimplemented anything
//! would be a second place for the canonical encoding to drift, which is the
//! one bug this whole layer exists to prevent.
//!
//! Neither binding opens a socket. TypeScript owns every byte that crosses the
//! network — CORS, tokens, interceptors and retries are all far more natural
//! there than through a Rust HTTP client compiled to wasm, and doing it once in
//! JS means one auth implementation rather than one per runtime.
//!
//! Build the browser artifact with `--features wasm --target
//! wasm32-unknown-unknown`, and the Node addon with `--features node`.

pub mod surface;

#[cfg(all(feature = "wasm", target_arch = "wasm32"))]
pub mod wasm;

#[cfg(all(feature = "node", not(target_arch = "wasm32")))]
pub mod node;
