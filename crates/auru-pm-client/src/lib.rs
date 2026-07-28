//! Client-facing API for connecting to Auru PM providers.
//!
//! Keeping this crate separate lets applications depend on the network client
//! without treating the core crate's current module layout as public API.

pub use auru_pm::token_store::{delete_token, load_token, store_token};
pub use auru_pm::{
    DeviceCodeResponse, HttpProvider, OAuthProgress, RegistryEntry, fetch_registry,
    resolve_endpoint, start_device_flow,
};
pub use auru_pm_protocol::WIRE_VERSION;
