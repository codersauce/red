#!/usr/bin/env python3
"""Export every Monaka60 part from monaka60.scad.

    python3 build.py                      # all printable STLs, DXFs and preview meshes
    python3 build.py --only stl/top       # just the matching outputs
    python3 build.py -D typing_angle=7    # overrides go to every part

Needs OpenSCAD 2021.01 or newer on PATH.
"""
import argparse
import concurrent.futures as cf
import os
import struct
import subprocess
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
SCAD = os.path.join(HERE, "monaka60.scad")

LAYOUTS = ["ansi", "tsangan", "wkl", "hhkb"]
TOP_STYLES = {"open": "none", "wkl": "wkl", "hhkb": "hhkb"}
LOW = {"$fn": "16"}          # lighter tessellation for preview-only meshes


def q(s):
    return f'"{s}"'


def jobs():
    j = [
        ("stl/bottom_left.stl", "bottom_left", {}),
        ("stl/bottom_right.stl", "bottom_right", {}),
        ("stl/bottom_left_udb.stl", "bottom_left", {"connector": q("udb")}),
        ("stl/gaskets_tpu.stl", "gaskets", {}),
        ("stl/foam_left_tpu.stl", "foam_left", {}),
        ("stl/foam_right_tpu.stl", "foam_right", {}),
        ("stl/test_switch.stl", "test_switch", {}),
        ("stl/test_joint.stl", "test_joint", {}),
        ("stl/test_gasket.stl", "test_gasket", {}),
        ("dxf/case_foam.dxf", "foam_2d", {}),
        ("dxf/gasket_pad.dxf", "gasket_2d", {}),
        ("preview/view_pcb.stl", "view_pcb", {}),
        ("preview/view_gaskets.stl", "view_gaskets", {}),
        ("preview/view_udb.stl", "view_udb", {"connector": q("udb")}),
        ("preview/view_foam.stl", "view_foam", {}),
    ]
    for style, blk in TOP_STYLES.items():
        for side in ("left", "right"):
            j.append((f"stl/top_{style}_{side}.stl", f"top_{side}", {"blockers": q(blk)}))
            j.append((f"preview/view_top_{style}_{side}.stl", f"view_top_{side}", {"blockers": q(blk)}))
    for lay in LAYOUTS:
        L = {"layout": q(lay)}
        j.append((f"dxf/plate_{lay}.dxf", "plate_2d", L))
        j.append((f"preview/view_switches_{lay}.stl", "view_switches", {**L, **LOW}))
        j.append((f"preview/view_keycaps_{lay}.stl", "view_keycaps", {**L, **LOW}))
        j.append((f"preview/view_keycaps_{lay}_pressed.stl", "view_keycaps_pressed", {**L, **LOW}))
        for s in ("alphas", "mods", "accents"):
            j.append((f"preview/view_keycaps_{lay}_{s}.stl", "view_keycaps", {**L, **LOW, "keycap_set": q(s)}))
        for side in ("left", "right"):
            j.append((f"stl/plate_{lay}_{side}.stl", f"plate_{side}", L))
            j.append((f"preview/view_plate_{lay}_{side}.stl", f"view_plate_{side}", L))
    return j


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
        f.write(b"Monaka60 binary STL".ljust(80, b"\0"))
        f.write(struct.pack("<I", len(tris)))
        for n, v in tris:
            f.write(struct.pack("<12fH", *n, *v[0], *v[1], *v[2], 0))


def run(job, overrides, outdir):
    out, part, extra = job
    path = os.path.join(outdir, out)
    os.makedirs(os.path.dirname(path), exist_ok=True)
    defs = {**extra, **overrides, "part": q(part)}
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
    ap.add_argument("--only", help="comma-separated output prefixes to build (e.g. stl/top,dxf)")
    args = ap.parse_args()
    overrides = dict(d.split("=", 1) for d in args.D)
    todo = [j for j in jobs() if not args.only or any(j[0].startswith(p) for p in args.only.split(","))]
    todo.sort(key=lambda j: not ("top" in j[0] or "bottom" in j[0]))      # slowest first
    failed = False
    with cf.ThreadPoolExecutor(max_workers=os.cpu_count() or 2) as pool:
        for out, problems in pool.map(lambda j: run(j, overrides, args.outdir), todo):
            print(("FAIL " if problems else "ok   ") + out)
            for p in problems:
                print("     " + p)
            failed |= bool(problems)
    sys.exit(1 if failed else 0)


if __name__ == "__main__":
    main()
