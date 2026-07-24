//! SSH tunnel support for zsql.
//!
//! This is the only crate in the workspace that depends on tokio or russh.
//! Both are private implementation details: no public item here names a
//! tokio or russh type, so a tunnel's future can be driven from any
//! executor (including the smol-based one the rest of the app uses) while
//! the tokio work happens on a dedicated background runtime owned by this
//! crate.

mod config;
mod error;
mod handler;
mod runtime;
mod tunnel;

pub use config::{HostKeyPolicy, SshAuth, SshConfig};
pub use error::SshError;
pub use tunnel::{SshTunnel, open_tunnel};
