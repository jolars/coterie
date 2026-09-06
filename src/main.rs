#[allow(
    dead_code,
    reason = "M1 defines CLI contracts before later milestones dispatch commands"
)]
mod cli;
mod config;
#[allow(
    dead_code,
    reason = "M1 defines stable identifiers before later milestones consume them"
)]
mod id;
mod project;
mod protocol;
mod providers;
mod state;
mod supervisor;
mod tasks;
mod transcript;
mod workspace;

fn main() {}
