//! Conversations over pNet.
//!
//! `discordium-client` and `discordium-server` are separate processes. They
//! share this library, register under those names, and use protocol
//! `application/discordium`.

mod create_list;
mod directory;
mod hello;
mod link;
mod node_api;
mod notice;
mod page;
mod post_history;
mod runtime;
mod startup;
mod store;

pub use directory::ProcessKind;
pub use startup::main_for;
