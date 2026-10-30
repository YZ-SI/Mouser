//! HID++ 1.0/2.0 protocol and device session over hidapi, ported from Mouser `core/hid_gesture.py`
//! (Windows path). Synchronous, thread-based: one worker thread owns the open device, callers use
//! [`HidListener`], events leave through a bounded channel of [`HidEvent`].
//!
//! Design rules (see `docs/DESIGN.md`): no async runtime, fixed-size report buffers, no HID++
//! traffic while idle, every volatile setting is re-applied after every link-up.

pub mod cache;
mod connect;
mod listener;
pub mod proto;
mod session;
pub mod transport;
mod types;

pub use listener::HidListener;
pub use transport::{Backend, HidapiBackend, IfaceInfo, IoError, Transport};
pub use types::*;

#[cfg(test)]
mod testutil;
#[cfg(test)]
mod tests;
