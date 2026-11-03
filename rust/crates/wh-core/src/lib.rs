//! The daemon's logic, ported from `daemon/src`. So far: the runner's pure parts (prompts, arguments, stream events) and the
//! runner policy. Still to come: config files, job records, workspace git, connection, supervisor.

pub mod config;
pub mod environment;
pub mod runner;
pub mod state;
