#[cfg(test)]
extern crate self as fluxrepo_update;

pub mod cli;
pub mod github;
pub mod models;
pub mod resolvers;
pub mod scanner;
pub mod update_run;
