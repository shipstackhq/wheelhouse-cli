//! The daemon's logic, ported from `daemon/src`. So far: the runner's pure parts (prompts, arguments, stream events) and the
//! runner policy. Still to come: config files, job records, workspace git, connection, supervisor.

pub mod cancel;
pub mod capabilities;
pub mod chat;
pub mod claude;
pub mod config;
pub mod connection;
pub mod daemon;
pub mod environment;
pub mod keystore;
pub mod runner;
pub mod runners;
pub mod selfcheck;
pub mod state;
pub mod supervisor;
pub mod workspace;
