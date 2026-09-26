//! The `recast-radar` command. See `docs/guide/cli.md`.

use std::process::ExitCode;

fn main() -> ExitCode {
    recast_radar_cli::main_with_args(std::env::args_os())
}
