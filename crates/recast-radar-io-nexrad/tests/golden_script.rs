//! `tools/level2_golden.py` reproduces every committed golden file.
//!
//! The Level II decoder tests compare against JSON goldens under
//! `testdata/level2/golden/`, written by `tools/level2_golden.py` with MetPy
//! 1.7.1 and Py-ART 2.2.5. This test runs the script in `--check` mode, which
//! regenerates every group in memory and fails unless each committed file is
//! reproduced byte for byte, no committed file is left unproduced, and no
//! file the script would write is missing.
//!
//! It is ignored by default because it needs that Python environment. Run it
//! with the interpreter in `RECAST_RADAR_GOLDEN_PYTHON`:
//!
//! ```text
//! RECAST_RADAR_GOLDEN_PYTHON=/path/to/python \
//!     cargo test -p recast-radar-io-nexrad --test golden_script -- --ignored
//! ```
//!
//! (`tools/ci/level2-golden-check.sh` does the same.) Every source file the
//! script reads is fetched into the testdata cache first; a file that cannot
//! be fetched fails the test instead of skipping it, because a partial check
//! proves nothing about the missing goldens.

use std::path::Path;
use std::process::Command;

const PYTHON_ENV: &str = "RECAST_RADAR_GOLDEN_PYTHON";

fn script() -> std::path::PathBuf {
    recast_radar_testdata::workspace_root().join("tools/level2_golden.py")
}

fn python(interpreter: &Path) -> Command {
    let mut command = Command::new(interpreter);
    command
        .arg(script())
        .env("RECAST_RADAR_TESTDATA", recast_radar_testdata::cache_dir())
        .env("PYTHONDONTWRITEBYTECODE", "1")
        .env("PYART_QUIET", "1");
    command
}

#[test]
#[ignore = "needs Python with MetPy 1.7.1 and Py-ART 2.2.5 in RECAST_RADAR_GOLDEN_PYTHON"]
fn golden_script_reproduces_every_committed_golden() {
    let interpreter = std::env::var_os(PYTHON_ENV).unwrap_or_else(|| {
        panic!("set {PYTHON_ENV} to a Python with MetPy 1.7.1 and arm_pyart 2.2.5")
    });
    let interpreter = Path::new(&interpreter);

    let listed = python(interpreter)
        .args(["--list-sources", "all"])
        .output()
        .unwrap_or_else(|error| panic!("{}: {error}", interpreter.display()));
    assert!(
        listed.status.success(),
        "--list-sources failed:\n{}",
        String::from_utf8_lossy(&listed.stderr)
    );
    let ids: Vec<String> = String::from_utf8(listed.stdout)
        .unwrap()
        .lines()
        .map(str::to_owned)
        .collect();
    // The default sources of the six groups: 32 distinct manifest ids.
    assert!(ids.len() >= 32, "sources: {ids:?}");
    for id in &ids {
        recast_radar_testdata::path(id).unwrap_or_else(|error| panic!("{id}: {error}"));
    }

    let status = python(interpreter)
        .args(["--check", "all"])
        .status()
        .unwrap_or_else(|error| panic!("{}: {error}", interpreter.display()));
    assert!(
        status.success(),
        "tools/level2_golden.py --check all failed ({status}); see the problems listed above"
    );
}
