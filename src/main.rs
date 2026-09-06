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
#[allow(
    dead_code,
    reason = "M1 defines durable state before later milestones start the supervisor"
)]
mod state;
mod supervisor;
#[allow(
    dead_code,
    reason = "M1 defines task contracts before later milestones expose task commands"
)]
mod tasks;
#[allow(
    dead_code,
    reason = "M1 defines transcript storage before provider sessions consume it"
)]
mod transcript;
mod workspace;

fn main() {}
