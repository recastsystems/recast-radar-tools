#!/usr/bin/env python3
"""Survey of the TDA adaptation carried by real TVS products (product 61).

Usage::

    python tools/level3_etvs_survey.py [--year 2022]

Standard library only. For every site prefix of the AWS bucket
``unidata-nexrad-level3``, reads the first ``SSS_NTV_<year>`` object (falling
back to 2021) and prints the TDA adaptation values "Max # of TVSs" and
"Max # of Elevated TVSs" from the product's adaptation page, then a count of
each pair. Packet 26 (ETVS) can only appear in a TVS product when the TDA
detects elevated TVSs, which a maximum of 0 disables
(``docs/level3/reference.md`` section 7). Two requests per site, 0.2 s apart.
"""

import argparse
import collections
import re
import time
import urllib.request

BUCKET = "https://unidata-nexrad-level3.s3.amazonaws.com/"


def get(url):
    with urllib.request.urlopen(url, timeout=60) as response:
        return response.read()


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--year", default="2022")
    args = parser.parse_args()
    sites = [s.decode() for s in re.findall(rb"<Prefix>([A-Z0-9]{3})_</Prefix>",
                                            get(BUCKET + "?list-type=2&delimiter=_"))]
    counts = collections.Counter()
    for site in sites:
        found = None
        for year in (args.year, "2021"):
            listing = get(BUCKET + f"?list-type=2&prefix={site}_NTV_{year}&max-keys=1")
            time.sleep(0.2)
            keys = re.findall(rb"<Key>([^<]+)</Key>", listing)
            if keys:
                key = keys[0].decode()
                data = get(BUCKET + key)
                time.sleep(0.2)
                etvs = re.search(rb"(\d+)\.+Max # of Elevated TVSs", data)
                tvs = re.search(rb"(\d+)\.+Max # of TVSs", data)
                found = (key, tvs.group(1).decode() if tvs else None,
                         etvs.group(1).decode() if etvs else None)
                break
        if found:
            counts[found[1:]] += 1
        print(site, found, flush=True)
    for (tvs, etvs), n in sorted(counts.items(), key=str):
        print(f"max TVSs {tvs}, max elevated TVSs {etvs}: {n} sites")


if __name__ == "__main__":
    main()
