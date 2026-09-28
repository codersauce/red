#!/usr/bin/env python3
"""Geometry checks for KB60. Run after build.py (same -D overrides).

    pip install numpy scipy trimesh shapely manifold3d
    python3 verify.py

Loads the exported meshes, puts everything in its assembled position and
checks fit, interference, clearances and bed size. Exits 1 if anything fails.
"""
import argparse
import json
import math
import os
import re
import subprocess
import sys

import manifold3d as m3d
import numpy as np
import trimesh
from shapely.geometry import LineString, Polygon
from shapely.ops import nearest_points, unary_union

HERE = os.path.dirname(os.path.abspath(__file__))
BED = 256.0          # Bambu Lab X1C build plate (mm)
TOL = 0.05           # mm^3; treat smaller boolean slivers as numerical noise

results = []


def check(name, ok, detail=""):
    results.append((name, bool(ok), detail))
    print(f"{'PASS' if ok else 'FAIL'}  {name}" + (f"  ({detail})" if detail else ""))


def info(overrides):
    cmd = ["openscad", "-o", "/dev/null", "--export-format", "stl", "-D", 'part="info"']
    for d in overrides:
        cmd += ["-D", d]
    cmd.append(os.path.join(HERE, "keyboard60.scad"))
    out = subprocess.run(cmd, capture_output=True, text=True)
    m = re.search(r"ECHO: INFO = (\[.*\])", out.stdout + out.stderr)
    if not m:
        sys.exit("could not read dimensions from keyboard60.scad:\n" + out.stderr)
    v = json.loads(m.group(1))
    keys = ["th", "Y0", "Z0", "w_pcb_bot", "w_pcb_top", "w_plate_bot", "w_plate_top", "w_rim",
            "case_w", "case_d", "split_x", "cav_w", "cav_d", "bezel_side", "pcb_clearance",
            "pcb_width", "pcb_depth", "holes", "usb", "front_h", "back_h", "standoff_h", "hole_depth", "foam_t"]
    return dict(zip(keys, v))


def load(rel):
    return trimesh.load(os.path.join(HERE, rel), process=True)


def man(mesh):
    return m3d.Manifold(m3d.Mesh(vert_properties=np.asarray(mesh.vertices, np.float32),
                                 tri_verts=np.asarray(mesh.faces, np.uint32)))


def vol(m):
    return m.volume() if hasattr(m, "volume") else m.get_volume()


def overlap(a, b):
    return vol(a ^ b)


def tilted_matrix(I):
    """4x4 transform from the tilted (u, v, w) frame to world coordinates."""
    t = math.radians(I["th"])
    M = np.eye(4)
    M[1, 1], M[1, 2] = math.cos(t), -math.sin(t)
    M[2, 1], M[2, 2] = math.sin(t), math.cos(t)
    M[1, 3], M[2, 3] = I["Y0"], I["Z0"]
    return M


def box_tilted(I, u0, u1, v0, v1, w0, w1):
    b = trimesh.creation.box(extents=[u1 - u0, v1 - v0, w1 - w0])
    b.apply_translation([(u0 + u1) / 2, (v0 + v1) / 2, (w0 + w1) / 2])
    b.apply_transform(tilted_matrix(I))
    return man(b)


def cyl_tilted(I, u, v, w0, w1, d):
    c = trimesh.creation.cylinder(radius=d / 2, height=w1 - w0, sections=48)
    c.apply_translation([u, v, (w0 + w1) / 2])
    c.apply_transform(tilted_matrix(I))
    return man(c)


def plate_web(mesh_rel):
    """Thinnest material in a plate half: min distance between any two outline rings.
    The plate is a straight extrusion, so its top faces are exactly its 2D shape."""
    m = load(mesh_rel)
    top = m.face_normals[:, 2] > 0.999
    shape = unary_union([Polygon(t) for t in m.triangles[top][:, :, :2]]).buffer(0)
    polys = list(shape.geoms) if shape.geom_type == "MultiPolygon" else [shape]
    rings = [LineString(r.coords) for p in polys for r in [p.exterior, *p.interiors]]
    best = (1e9, None)
    for i in range(len(rings)):
        for j in range(i + 1, len(rings)):
            d = rings[i].distance(rings[j])
            if d < best[0]:
                a, b = nearest_points(rings[i], rings[j])
                best = (d, (round((a.x + b.x) / 2, 1), round((a.y + b.y) / 2, 1)))
    return best, len(polys)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("-D", action="append", default=[])
    args = ap.parse_args()
    I = info(args.D)
    print(f"case {I['case_w']:.1f} x {I['case_d']:.1f} mm, front {I['front_h']:.1f} mm, "
          f"back {I['back_h']:.1f} mm, typing angle {I['th']} deg\n")

    # ---------------------------------------------------------------- meshes
    printable = ["case_left", "case_right", "case_left_selftap", "case_right_selftap",
                 "plate_left", "plate_right", "plate_left_tsangan", "plate_right_tsangan",
                 "foam_left_tpu", "foam_right_tpu", "test_switch", "test_joint"]
    meshes = {}
    for n in printable:
        m = load(f"stl/{n}.stl")
        meshes[n] = m
        mm = man(m)
        st = mm.status()
        ext = np.ptp(m.bounds, axis=0)
        check(f"{n}: watertight, valid solid", m.is_watertight and m.is_volume and st == m3d.Error.NoError,
              f"{len(m.faces)} tris, {m.volume / 1000:.1f} cm3")
        check(f"{n}: fits X1C build volume", max(ext) <= BED,
              f"{ext[0]:.1f} x {ext[1]:.1f} x {ext[2]:.1f} mm")
        lowest = m.vertices[:, 2].min()
        down = (m.face_normals[:, 2] < -0.999) & (np.abs(m.triangles_center[:, 2] - lowest) < 1e-3)
        bottom = m.area_faces[down].sum()
        check(f"{n}: sits flat on the bed", abs(lowest) < 1e-3 and bottom > 100,
              f"first-layer contact {bottom / 100:.0f} cm2")

    ext_l = np.ptp(meshes["case_left"].bounds, axis=0)
    ext_r = np.ptp(meshes["case_right"].bounds, axis=0)
    check("both case halves fit on one plate",
          max(ext_l[0], ext_r[0]) + 10 <= BED and ext_l[1] + ext_r[1] + 10 + 10 <= BED,
          f"{max(ext_l[0], ext_r[0]):.1f} x {ext_l[1] + ext_r[1] + 10:.1f} mm incl. 10 mm gap")
    ext_pl = np.ptp(meshes["plate_left"].bounds, axis=0)
    ext_pr = np.ptp(meshes["plate_right"].bounds, axis=0)
    check("both plate halves fit on one plate",
          max(ext_pl[0], ext_pr[0]) + 10 <= BED and ext_pl[1] + ext_pr[1] + 10 + 10 <= BED,
          f"{max(ext_pl[0], ext_pr[0]):.1f} x {ext_pl[1] + ext_pr[1] + 10:.1f} mm incl. 10 mm gap")

    # -------------------------------------------------------------- assembly
    L, R = man(meshes["case_left"]), man(meshes["case_right"])
    case = L + R
    view = {n: man(load(f"preview/view_{n}.stl"))
            for n in ["pcb", "plate_left", "plate_right", "switches", "keycaps", "keycaps_pressed", "foam"]}

    check("case halves do not collide", overlap(L, R) < TOL, f"{overlap(L, R):.3f} mm3")
    t = math.radians(I["th"])
    # the foam lies on the floor; lift it 0.01 mm so face contact isn't counted as a collision
    view["foam"] = view["foam"].translate([0, -0.01 * math.sin(t), 0.01 * math.cos(t)])
    for n in ["pcb", "plate_left", "plate_right", "switches", "keycaps", "keycaps_pressed", "foam"]:
        o = overlap(view[n], case)
        check(f"{n.replace('_', ' ')} clears the case", o < TOL, f"{o:.3f} mm3")
    plate = view["plate_left"] + view["plate_right"]
    check("plate halves do not collide", overlap(view["plate_left"], view["plate_right"]) < TOL)
    check("plate clears PCB", overlap(plate, view["pcb"]) < TOL)
    check("pressed keycaps clear the plate", overlap(view["keycaps_pressed"], plate) < TOL)
    gap = I["w_pcb_bot"] - I["foam_t"]
    check("foam leaves room for hot-swap sockets (1.85 mm) under the PCB",
          overlap(view["foam"], view["pcb"]) < TOL and gap >= 1.9, f"{gap:.1f} mm gap above {I['foam_t']} mm foam")

    # PCB rests on all six standoffs: drop it 0.05 mm and it must touch
    drop = view["pcb"].translate([0, 0.05 * math.sin(t), -0.05 * math.cos(t)])
    contacts = [overlap(drop, cyl_tilted(I, u, v, I["w_pcb_bot"] - 1, I["w_pcb_bot"] + 1, 6.5) ^ case)
                for u, v in I["holes"]]
    check("PCB sits on all 6 standoffs", all(c > 0.05 for c in contacts),
          "contact per standoff (mm3 at 0.05 mm drop): " + ", ".join(f"{c:.2f}" for c in contacts))

    # Screw path: an M2 pan head (4.0 mm) + driver passes the plate; M2 shank passes PCB hole into the insert
    for i, (u, v) in enumerate(I["holes"], 1):
        head = cyl_tilted(I, u, v, I["w_pcb_top"] + 0.01, I["w_rim"] + 20, 4.0)
        shank = cyl_tilted(I, u, v, I["w_pcb_bot"] - I["hole_depth"] + 1.5, I["w_pcb_top"] + 1, 1.9)
        oh, os_ = overlap(head, plate), overlap(shank, case + view["pcb"])
        check(f"screw {i}: head/driver clears plate, shank enters PCB hole + standoff", oh < TOL and os_ < TOL,
              f"u={u:.1f} v={v:.1f}")

    # -------------------------------------------------------------- dovetails
    free = {"+0.1 Y": (0, 0.1, 0), "-0.1 Y": (0, -0.1, 0), "+0.1 Z": (0, 0, 0.1), "-0.1 X": (-0.1, 0, 0)}
    for label, d in free.items():
        o = overlap(L.translate(list(d)), R)
        check(f"seam keys: {label} of slack, no contact", o < TOL, f"{o:.3f} mm3")
    locked = {lab: overlap(L.translate(list(d)), R) for lab, d in
              {"pulled apart 0.2 X": (-0.2, 0, 0), "shifted 0.2 Y": (0, 0.2, 0), "shifted -0.2 Y": (0, -0.2, 0)}.items()}
    check("seam keys lock the halves (seam can open < 0.2 mm)", all(v > 1 for v in locked.values()),
          ", ".join(f"{k}: {v:.1f} mm3" for k, v in locked.items()))

    # ------------------------------------------------------------ USB access
    ua, ub = I["usb"]
    u_off = I["bezel_side"] + I["pcb_clearance"]
    v_edge = I["pcb_clearance"] + I["pcb_depth"]
    fails = []
    centers = np.arange(ua + 7, ub - 7 + 1e-6, 1.0) if ub - ua > 14 else [(ua + ub) / 2]
    for c in centers:
        for wc in (I["w_pcb_bot"] - 1.6, I["w_pcb_bot"] + 0.8):     # bottom-mounted / mid-mount receptacle
            plug = box_tilted(I, u_off + c - 6.2, u_off + c + 6.2, v_edge + 0.3, v_edge + 40, wc - 3.25, wc + 3.25)
            if overlap(plug, case) > TOL:
                fails.append((c, wc))
    check("USB-C plug overmold (12.4 x 6.5 mm) reaches the PCB edge", not fails,
          f"port centres {centers[0]:.1f}-{centers[-1]:.1f} mm from PCB left edge, "
          f"bottom-mount and mid-mount" + (f"; blocked: {fails[:4]}" if fails else ""))

    # ---------------------------------------------------------------- plates
    for n in ["plate_left", "plate_right", "plate_left_tsangan", "plate_right_tsangan"]:
        (d, where), npoly = plate_web(f"stl/{n}.stl")
        check(f"{n}: thinnest web >= 0.9 mm, one piece", d >= 0.9 and npoly == 1, f"{d:.2f} mm near {where}")

    failed = [r for r in results if not r[1]]
    print(f"\n{len(results) - len(failed)}/{len(results)} checks passed")
    sys.exit(1 if failed else 0)


if __name__ == "__main__":
    main()
