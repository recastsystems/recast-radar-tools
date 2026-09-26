"""Step 1: decode every cached upstream frame (section 2.1) with the
feeds_survey example: each part alone, then the frame's parts merged as the
poller does (`--merge`, first part as the base). A frame is one
FEEDS_ROOT/<provider>/<site>/ directory as `feeds_plans fetch` writes it,
with a frame.json that lists the parts in plan order; `dwd-dbzh` is the DWD
frame planned with `filtered_reflectivity(true)`. Directories without a
frame.json hold files fetched by hand (DWD's boo-dbzh, SHMU's skjav-extra,
ORD's per-site listings and extra sweeps) and are skipped. IMGW's Cartesian products (FEEDS_ROOT/imgw/<site>/, no
frame.json) are decoded part by part only.

No network. Writes WORK/upstream_survey.jsonl.

Usage: python tools/feeds_survey/run_upstream_survey.py
"""

import json
import os

from common import FEEDS_ROOT, run_survey, work

PROVIDERS = [
    "dwd", "dwd-dbzh", "chmi", "shmu", "smhi", "fmi", "dmi", "meteoromania", "kaia", "arpa-piemonte",
    "arpa-lombardia", "geosphere", "ord", "jma", "australia-nci", "imgw",
]
with open(work("upstream_survey.jsonl"), "w", encoding="utf-8") as out:
    for provider in PROVIDERS:
        provider_dir = os.path.join(FEEDS_ROOT, provider)
        if not os.path.isdir(provider_dir):
            continue
        for site in sorted(os.listdir(provider_dir)):
            site_dir = os.path.join(provider_dir, site)
            frame = os.path.join(site_dir, "frame.json")
            if os.path.exists(frame):
                with open(frame, encoding="utf-8") as handle:
                    names = json.load(handle)["files"]
            elif provider == "imgw":
                names = sorted(
                    name for name in os.listdir(site_dir)
                    if os.path.isfile(os.path.join(site_dir, name)) and not name.endswith(".part")
                )
            else:
                print(f"{provider}:{site} skipped (no frame.json)", flush=True)
                continue
            parts = [os.path.join(site_dir, name) for name in names]
            pair = f"{provider}:{site}"
            jma = ["--jma-site", site] if provider == "jma" else []
            for row in run_survey(jma + parts):
                row.update(pair=pair, kind="part", path=row.get("file"))
                out.write(json.dumps(row) + "\n")
            if provider == "imgw":
                continue
            for row in run_survey(jma + ["--merge", pair] + parts):
                row.update(pair=pair, kind="merged")
                out.write(json.dumps(row) + "\n")
            print(pair, len(parts), "parts", flush=True)
print("->", work("upstream_survey.jsonl"))
