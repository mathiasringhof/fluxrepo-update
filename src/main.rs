use std::process::ExitCode;

use fluxrepo_update::cli::{self, EXIT_STRICT_FAILURE};

fn main() -> ExitCode {
    match cli::run() {
        Ok(code) => ExitCode::from(code),
        Err(error) => {
            eprintln!("{error:#}");
            ExitCode::from(EXIT_STRICT_FAILURE)
        }
    }
}
