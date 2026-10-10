//! The `shika` command a Lead runs. The app binary is the client: `main`
//! calls [`run`] before GPUI, settings, or app data are touched, and exits
//! with its status. It only talks to the socket named by the environment.
use shika_core::control::{self, Request};
use std::path::Path;

/// Runs one `shika <command>` and returns the process exit status: 0 on
/// success, 1 on a refusal, 2 on a usage, environment, or connection error.
pub fn run(args: &[String]) -> i32 {
    let (Some(socket), Some(token)) = (
        std::env::var_os("SHIKA_SOCKET"),
        std::env::var("SHIKA_TOKEN").ok(),
    ) else {
        eprintln!("shika commands run inside a Shika Lead or Lead-started worker terminal.");
        return 2;
    };
    let (command, json) = match control::parse_args(args) {
        Ok(parsed) => parsed,
        Err(message) => {
            eprintln!("{message}");
            return 2;
        }
    };
    match control::send(Path::new(&socket), &Request::new(token, command)) {
        Ok(reply) => {
            if json {
                match serde_json::to_string(&reply) {
                    Ok(line) => println!("{line}"),
                    Err(err) => {
                        eprintln!("{err}");
                        return 2;
                    }
                }
            } else {
                println!("{}", control::render_text(&reply));
            }
            reply.exit_code()
        }
        Err(err) => {
            eprintln!("{err}");
            2
        }
    }
}
