#![allow(
    clippy::to_string_in_format_args,
    clippy::useless_format,
    clippy::manual_inspect
)]
//! Agent runtime — orchestration, security, observability, cron, SOP, skills, hardware, and more.

#![recursion_limit = "256"]

pub mod channel;
pub mod cli_input;
pub mod identity;
pub mod migration;
pub mod util;

pub mod agent;
pub mod approval;
pub mod browse;
pub mod cost;
pub mod cron;
pub mod daemon;
pub mod doctor;
pub mod dt_nodes_registry;
pub mod health;
pub mod heartbeat;
pub mod hooks;
pub mod i18n;
pub(crate) mod i18n_bootstrap;
pub(crate) mod i18n_errors;
pub(crate) mod i18n_loader;
pub mod integrations;
pub mod nodes;
pub mod observability;
pub mod peers;
pub mod platform;
pub mod process_stats;
pub mod quickstart;
pub mod rag;
pub mod routines;
pub mod rpc;
pub mod security;
pub mod service;
pub mod skillforge;
pub mod skills;
pub mod sop;
pub mod subagent;
pub mod tools;
pub mod trust;
pub mod tunnel;
pub mod verifiable_intent;
