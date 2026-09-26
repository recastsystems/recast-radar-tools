#!/usr/bin/env python3
"""Survey the product IDs of the NCEI Level III archive.

Usage (from anywhere)::

    python tools/level3_ncei_survey.py OUT.jsonl YEAR [YEAR ...]
    python tools/level3_ncei_survey.py --summary OUT.jsonl

Lists every day archive of the given years in the Google Cloud copy of the
NCEI archive (bucket ``gcp-public-data-nexrad-l3``, objects
``YYYY/MM/DD/SITE/NWS_NEXRAD_NXL3_SITE_YYYYMMDD000000_YYYYMMDD235959.tar.Z``
or ``.tar.gz``), streams each through ``curl``, ``gzip -dc`` and ``tar -t``
(nothing is kept on disk) and appends one JSON line per archive to
``OUT.jsonl``: ``{"name": object, "ids": {"SDUS:N0R": count, ...}}``, the
WMO heading prefix and the three-character AWIPS product ID of each member
name (``KLOT_SDUS53_084LOT_199311200721``: ``SDUS:084``). Archives already
in ``OUT.jsonl`` are skipped, so a run can be resumed. Archives smaller than
2000 bytes (empty days) are skipped.

``--summary`` prints, per product ID, the members and archives holding it.
The products 1993-2001 the corpus took from this survey have numeric IDs:
the NCEI archive names products without an AWIPS ID by their code
(``016``, ``050``, ``084``, ``101``).

Standard library only; needs ``curl``, ``gzip`` and ``tar`` on the path.
"""

import collections
import json
import subprocess
import sys
import urllib.parse
import urllib.request

BUCKET = "gcp-public-data-nexrad-l3"


def objects(prefix):
    token = None
    while True:
        query = {"prefix": prefix, "fields": "items(name,size),nextPageToken",
                 "maxResults": "1000"}
        if token:
            query["pageToken"] = token
        url = (f"https://storage.googleapis.com/storage/v1/b/{BUCKET}/o?"
               + urllib.parse.urlencode(query))
        with urllib.request.urlopen(url) as response:
            data = json.load(response)
        for item in data.get("items", []):
            yield int(item["size"]), item["name"]
        token = data.get("nextPageToken")
        if not token:
            return


def product_ids(name):
    url = f"https://storage.googleapis.com/{BUCKET}/{name}"
    listing = subprocess.run(f'curl -s "{url}" | gzip -dc | tar -t', shell=True,
                             capture_output=True, text=True).stdout
    ids = collections.Counter()
    for member in listing.split():
        parts = member.split("_")
        if len(parts) >= 3:
            ids[parts[1][:4] + ":" + parts[2][:3]] += 1
    return ids


def summary(path):
    members = collections.Counter()
    archives = collections.defaultdict(set)
    with open(path) as lines:
        for line in lines:
            item = json.loads(line)
            for key, count in item["ids"].items():
                pid = key.split(":")[1]
                members[pid] += count
                archives[pid].add(item["name"].split("/")[-1][16:29])
    for pid in sorted(members):
        sample = ", ".join(sorted(archives[pid])[:6])
        print(f"{pid}\t{members[pid]}\t{len(archives[pid])}\t{sample}")


def main():
    if len(sys.argv) >= 3 and sys.argv[1] == "--summary":
        summary(sys.argv[2])
        return
    if len(sys.argv) < 3:
        sys.exit(__doc__)
    out_path, years = sys.argv[1], sys.argv[2:]
    done = set()
    try:
        with open(out_path) as lines:
            done = {json.loads(line)["name"] for line in lines if line.strip()}
    except FileNotFoundError:
        pass
    with open(out_path, "a") as out:
        for year in years:
            for size, name in objects(f"{year}/"):
                if size < 2000 or name in done:
                    continue
                out.write(json.dumps({"name": name, "ids": product_ids(name)}) + "\n")
                out.flush()


if __name__ == "__main__":
    main()
