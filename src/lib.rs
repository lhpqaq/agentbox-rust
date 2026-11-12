#[cfg(not(any(target_os = "linux", target_os = "macos")))]
compile_error!("AgentBox Rust supports Linux and macOS only.");
pub mod broker;
pub mod config;
pub mod error;
pub mod http;
pub mod mcp;
pub mod rpc;
pub mod store;
pub mod terminal;
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

mod platform;

pub mod selection;

pub mod transcript;

pub mod doctor;

pub mod shell;
