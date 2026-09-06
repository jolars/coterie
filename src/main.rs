mod auth;
#[allow(
    dead_code,
    reason = "M1 defines CLI contracts before later milestones dispatch commands"
)]
mod cli;
#[allow(
    dead_code,
    reason = "M1 defines configuration policy before later milestones resolve it"
)]
mod config;
#[allow(
    dead_code,
    reason = "M1 defines stable identifiers before later milestones consume them"
)]
mod id;
#[allow(
    dead_code,
    reason = "M2 defines project discovery before supervisor startup consumes it"
)]
mod project;
mod protocol;
#[allow(
    dead_code,
    reason = "M2 defines provider lifecycles before delegation commands launch them"
)]
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

#[tokio::main(flavor = "current_thread")]
async fn main() -> std::process::ExitCode {
    match supervisor::run_from_environment().await {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("coterie: {error}");
            std::process::ExitCode::FAILURE
        }
    }
}
