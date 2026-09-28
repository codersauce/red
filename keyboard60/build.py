#!/usr/bin/env python3
"""Export every KB60 part from keyboard60.scad.

    python3 build.py              # printable STLs, plate DXFs, preview meshes
    python3 build.py -D typing_angle=7 -D 'usb_style="custom"' -D usb_center=31

Extra -D overrides are passed to OpenSCAD for every part, so the whole set
stays consistent. Needs OpenSCAD on PATH (2021.01 or newer).
"""
import argparse
import concurrent.futures as cf
import os
import struct
import subprocess
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
SCAD = os.path.join(HERE, "keyboard60.scad")

# (output path, part, extra -D overrides)
JOBS = [
    ("stl/case_left.stl", "case_left", {}),
    ("stl/case_right.stl", "case_right", {}),
    ("stl/plate_left.stl", "plate_left", {}),
    ("stl/plate_right.stl", "plate_right", {}),
    ("stl/plate_left_tsangan.stl", "plate_left", {"layout": '"tsangan"'}),
    ("stl/plate_right_tsangan.stl", "plate_right", {"layout": '"tsangan"'}),
    ("stl/case_left_selftap.stl", "case_left", {"standoff_hole_diameter": "1.8"}),
    ("stl/case_right_selftap.stl", "case_right", {"standoff_hole_diameter": "1.8"}),
    ("stl/foam_left_tpu.stl", "foam_left", {}),
    ("stl/foam_right_tpu.stl", "foam_right", {}),
    ("dxf/case_foam.dxf", "foam_2d", {}),
    ("stl/test_switch.stl", "test_switch", {}),
    ("stl/test_joint.stl", "test_joint", {}),
    ("dxf/plate_ansi.dxf", "plate_2d", {}),
    ("dxf/plate_tsangan.dxf", "plate_2d", {"layout": '"tsangan"'}),
    ("preview/view_plate_left.stl", "view_plate_left", {}),
    ("preview/view_plate_right.stl", "view_plate_right", {}),
    ("preview/view_pcb.stl", "view_pcb", {}),
    ("preview/view_foam.stl", "view_foam", {}),
    ("preview/view_keycaps_alphas.stl", "view_keycaps", {"$fn": "16", "keycap_set": '"alphas"'}),
    ("preview/view_keycaps_mods.stl", "view_keycaps", {"$fn": "16", "keycap_set": '"mods"'}),
    ("preview/view_keycaps_accents.stl", "view_keycaps", {"$fn": "16", "keycap_set": '"accents"'}),
    ("preview/view_switches.stl", "view_switches", {"$fn": "16"}),
    ("preview/view_keycaps.stl", "view_keycaps", {"$fn": "16"}),
    ("preview/view_keycaps_pressed.stl", "view_keycaps_pressed", {"$fn": "16"}),
]


def ascii_to_binary_stl(path):
    """OpenSCAD 2021 writes ASCII STL; binary is ~5x smaller and loads faster."""
    with open(path, "rb") as f:
        if f.read(5) != b"solid":
            return
    tris, normal, verts = [], None, []
    with open(path) as f:
        for line in f:
            t = line.split()
            if not t:
                continue
            if t[0] == "facet":
                normal, verts = tuple(map(float, t[2:5])), []
            elif t[0] == "vertex":
                verts.append(tuple(map(float, t[1:4])))
            elif t[0] == "endfacet":
                tris.append((normal, verts))
    with open(path, "wb") as f:
        f.write(b"KB60 binary STL".ljust(80, b"\0"))
        f.write(struct.pack("<I", len(tris)))
        for n, v in tris:
            f.write(struct.pack("<12fH", *n, *v[0], *v[1], *v[2], 0))


def run(job, overrides, outdir):
    out, part, extra = job
    path = os.path.join(outdir, out)
    os.makedirs(os.path.dirname(path), exist_ok=True)
    defs = {**extra, **overrides, "part": f'"{part}"'}
    cmd = ["openscad", "-o", path]
    for k, v in defs.items():
        cmd += ["-D", f"{k}={v}"]
    cmd.append(SCAD)
    res = subprocess.run(cmd, capture_output=True, text=True)
    log = res.stdout + res.stderr
    problems = [l for l in log.splitlines() if "WARNING" in l or "ERROR" in l]
    if res.returncode != 0 or not os.path.exists(path):
        problems.append(f"openscad exited {res.returncode}")
    elif path.endswith(".stl"):
        ascii_to_binary_stl(path)
    return out, problems


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("-D", action="append", default=[], metavar="NAME=VALUE", help="OpenSCAD override")
    ap.add_argument("-o", "--outdir", default=HERE)
    ap.add_argument("--only", help="comma-separated output prefixes to build (e.g. stl/case)")
    args = ap.parse_args()
    overrides = dict(d.split("=", 1) for d in args.D)
    jobs = [j for j in JOBS if not args.only or any(j[0].startswith(p) for p in args.only.split(","))]
    failed = False
    with cf.ThreadPoolExecutor(max_workers=os.cpu_count() or 2) as pool:
        for out, problems in pool.map(lambda j: run(j, overrides, args.outdir), jobs):
            print(("FAIL " if problems else "ok   ") + out)
            for p in problems:
                print("     " + p)
            failed |= bool(problems)
    sys.exit(1 if failed else 0)


if __name__ == "__main__":
    main()
