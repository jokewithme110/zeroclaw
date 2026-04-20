//! Node subsystem for node-control.

pub mod node_registry;
pub mod ws_node;

pub use node_registry::{ConnectedNodeRegistry, NodeCommandResult};
pub use ws_node::handle_ws_node;
