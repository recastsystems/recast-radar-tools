"""Paths and helpers shared by the feed-survey scripts.

The survey is written up in docs/testdata/feeds-survey.md; section 5 says in
which order to run these scripts. Every script reads and writes local files
only.

Environment (all optional):

- FEEDS_SURVEY_FEEDS: upstream cache root (default ~/radar-corpus/feeds)
- FEEDS_SURVEY_WORK: output directory (default <repo>/target/feeds-survey)
- FEEDS_SURVEY_EXE: the feeds_survey example binary (default
  <repo>/target/release/examples/feeds_survey[.exe]; build it with
  `cargo build --release -p recast-radar-data --example feeds_survey`)
"""

import json
import os
import subprocess
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
REPO = os.path.dirname(os.path.dirname(HERE))
FEEDS_ROOT = os.environ.get("FEEDS_SURVEY_FEEDS", os.path.join(os.path.expanduser("~"), "radar-corpus", "feeds"))
WORK = os.environ.get("FEEDS_SURVEY_WORK", os.path.join(REPO, "target", "feeds-survey"))

os.makedirs(WORK, exist_ok=True)


def work(name):
    """Path of an output file in WORK."""
    return os.path.join(WORK, name)


def survey_exe():
    """The feeds_survey example binary."""
    if os.environ.get("FEEDS_SURVEY_EXE"):
        return os.environ["FEEDS_SURVEY_EXE"]
    name = "feeds_survey.exe" if sys.platform == "win32" else "feeds_survey"
    return os.path.join(REPO, "target", "release", "examples", name)


def run_survey(args):
    """Run feeds_survey and return its JSON lines."""
    done = subprocess.run([survey_exe()] + list(args), capture_output=True, text=True, check=False)
    if done.stderr.strip():
        sys.stderr.write(done.stderr[-2000:])
    return [json.loads(line) for line in done.stdout.splitlines() if line.strip()]


def read_jsonl(name):
    with open(work(name), encoding="utf-8") as handle:
        return [json.loads(line) for line in handle if line.strip()]
