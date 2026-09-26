//! Print the verified local path of each testdata id, one per line.
//!
//! ```text
//! testdata-path ID...
//! ```
//!
//! Files that are neither committed nor cached are downloaded into the cache
//! first (with the default `download` feature), exactly as the tests resolve
//! them through `recast_radar_testdata::path`. Scripts that run programs on
//! the corpus use it, such as `tools/check_example_outputs.py`.

use std::process::ExitCode;

const USAGE: &str = "usage: testdata-path ID...";

fn main() -> ExitCode {
    let ids: Vec<String> = std::env::args().skip(1).collect();
    if ids.is_empty() || ids.iter().any(|id| id == "-h" || id == "--help") {
        eprintln!("{USAGE}");
        return ExitCode::FAILURE;
    }
    for id in &ids {
        match recast_radar_testdata::path(id) {
            Ok(path) => println!("{}", path.display()),
            Err(error) => {
                eprintln!("testdata-path: {id}: {error}");
                return ExitCode::FAILURE;
            }
        }
    }
    ExitCode::SUCCESS
}
