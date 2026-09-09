mod auth;
mod cli;
#[allow(
    dead_code,
    reason = "M5 defines configuration loading before runtime snapshot integration"
)]
mod config;
mod doctor;
mod fault;
#[allow(
    dead_code,
    reason = "M1 defines stable identifiers before later milestones consume them"
)]
mod id;
mod private_fs;
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
mod redaction;
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

use clap::Parser;

#[tokio::main(flavor = "current_thread")]
async fn main() -> std::process::ExitCode {
    let raw_arguments = std::env::args_os().collect::<Vec<_>>();
    let json_requested = raw_arguments
        .iter()
        .skip(1)
        .any(|argument| argument == "--json");
    let arguments = match cli::Arguments::try_parse_from(raw_arguments) {
        Ok(arguments) => arguments,
        Err(error) if error.exit_code() == 0 => {
            let _ = error.print();
            return std::process::ExitCode::SUCCESS;
        }
        Err(error) if json_requested => {
            let diagnostic = cli::Diagnostic::new(
                cli::ErrorCode::InvalidArgument,
                error.to_string().trim().to_owned(),
            );
            let mut stdout = std::io::stdout().lock();
            let mut stderr = std::io::stderr().lock();
            return match cli::render_json_error(
                &mut stdout,
                &mut stderr,
                &diagnostic,
            ) {
                Ok(category) => category.into(),
                Err(render_error) => {
                    eprintln!("coterie: {render_error}");
                    std::process::ExitCode::FAILURE
                }
            };
        }
        Err(error) => {
            let exit_code = u8::try_from(error.exit_code()).unwrap_or(2);
            eprint!("{}", redaction::text(&error.to_string()));
            return std::process::ExitCode::from(exit_code);
        }
    };
    let json_output = arguments.json;
    match supervisor::run(arguments).await {
        Ok(category) => category.into(),
        Err(error) => {
            let diagnostic = error.diagnostic();
            let mut stdout = std::io::stdout().lock();
            let mut stderr = std::io::stderr().lock();
            let rendered = if json_output {
                cli::render_json_error(&mut stdout, &mut stderr, &diagnostic)
            } else {
                cli::render_human_error(&mut stdout, &mut stderr, &diagnostic)
            };
            match rendered {
                Ok(category) => category.into(),
                Err(render_error) => {
                    eprintln!("coterie: {render_error}");
                    std::process::ExitCode::FAILURE
                }
            }
        }
    }
}
