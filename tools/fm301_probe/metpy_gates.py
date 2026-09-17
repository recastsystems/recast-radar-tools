"""Print the native per-moment gate geometry of every sweep in a Level II file.

usage: metpy_gates.py <level2 file (.gz accepted)>

Uses MetPy's Level2File as an independent reader: for each sweep, every moment's
(first gate km, gate width km, gate count, word size, scale, offset) and how many
rays carry it. Used by docs/design/fm301-model.md appendix A.
"""
import sys
import warnings
from collections import Counter

warnings.filterwarnings("ignore")
from metpy.io import Level2File  # noqa: E402

f = Level2File(sys.argv[1])
print("nsweeps:", len(f.sweeps))
for i, sweep in enumerate(f.sweeps):
    geoms = {}
    for ray in sweep:
        moments = ray[-1]
        for name, (hdr, _data) in moments.items():
            key = name.decode().strip() if isinstance(name, bytes) else str(name)
            g = (
                round(float(hdr.first_gate), 4),
                round(float(hdr.gate_width), 4),
                int(hdr.num_gates),
                getattr(hdr, "data_size", None),
                getattr(hdr, "scale", None),
                getattr(hdr, "offset", None),
            )
            geoms.setdefault(key, Counter())[g] += 1
    parts = []
    for k, c in geoms.items():
        parts.append(
            f"{k}:"
            + ",".join(
                f"{g[0]}km+{g[1]}km x{g[2]} (bits={g[3]},scale={g[4]},off={g[5]}) n={n}"
                for g, n in c.most_common(3)
            )
        )
    print(f"sweep {i}: rays={len(sweep)} " + " | ".join(parts))
