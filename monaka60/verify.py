#!/usr/bin/env python3
"""Geometry checks for Monaka60. Run after build.py (with the same -D overrides).

    pip install numpy scipy trimesh shapely manifold3d
    python3 verify.py

Loads the exported meshes, puts every part where it sits in the finished
keyboard and checks fit, interference, clearances and bed size for all four
layouts. Exits 1 if anything fails.
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
BED = 256.0                      # Bambu Lab X1C build plate (mm)
TOL = 0.05                       # mm^3; boolean slivers below this are numerical noise
LAYOUTS = {"ansi": "open", "tsangan": "open", "wkl": "wkl", "hhkb": "hhkb"}
results = []


def check(name, ok, detail=""):
    results.append((name, bool(ok), detail))
    print(f"{'PASS' if ok else 'FAIL'}  {name}" + (f"  ({detail})" if detail else ""))


def info(overrides):
    cmd = ["openscad", "-o", "/dev/null", "--export-format", "stl", "-D", 'part="info"']
    for d in overrides:
        cmd += ["-D", d]
    cmd.append(os.path.join(HERE, "monaka60.scad"))
    out = subprocess.run(cmd, capture_output=True, text=True)
    m = re.search(r"ECHO: INFO = (\[.*\])", out.stdout + out.stderr)
    if not m:
        sys.exit("could not read dimensions from monaka60.scad:\n" + out.stderr)
    keys = ["th", "Y0", "Z0", "w_pcb_bot", "w_pcb_top", "w_plate_bot", "w_plate_top", "w_rim", "w_part",
            "w_seat", "w_ceiling", "w_blocker", "case_w", "case_d", "split_x", "cav_w", "cav_d", "bezel_side",
            "u_p", "v_p", "pcb_width", "pcb_depth", "usb", "front_h", "back_h", "screws", "gasket_t", "gasket_c",
            "udb", "tab_clearance", "n_keys", "foam_t", "pad", "tabs"]
    return dict(zip(keys, json.loads(m.group(1))))


_cache = {}


def load(rel):
    if rel not in _cache:
        _cache[rel] = trimesh.load(os.path.join(HERE, rel), process=True)
    return _cache[rel]


def man(mesh):
    return m3d.Manifold(m3d.Mesh(vert_properties=np.asarray(mesh.vertices, np.float32),
                                 tri_verts=np.asarray(mesh.faces, np.uint32)))


def M(rel):
    return man(load(rel))


def vol(m):
    return m.volume() if hasattr(m, "volume") else m.get_volume()


def overlap(a, b):
    return vol(a ^ b)


def tilt(I):
    t = math.radians(I["th"])
    T = np.eye(4)
    T[1, 1], T[1, 2], T[2, 1], T[2, 2] = math.cos(t), -math.sin(t), math.sin(t), math.cos(t)
    T[1, 3], T[2, 3] = I["Y0"], I["Z0"]
    return T


def along(I, du=0.0, dv=0.0, dw=0.0):
    """World translation for a move of (du, dv, dw) in the tilted frame."""
    t = math.radians(I["th"])
    return [du, dv * math.cos(t) - dw * math.sin(t), dv * math.sin(t) + dw * math.cos(t)]


def box_t(I, u0, u1, v0, v1, w0, w1):
    b = trimesh.creation.box(extents=[u1 - u0, v1 - v0, w1 - w0])
    b.apply_translation([(u0 + u1) / 2, (v0 + v1) / 2, (w0 + w1) / 2])
    b.apply_transform(tilt(I))
    return man(b)


def cyl_z(x, y, z0, z1, d):
    c = trimesh.creation.cylinder(radius=d / 2, height=z1 - z0, sections=48)
    c.apply_translation([x, y, (z0 + z1) / 2])
    return man(c)


def plate_web(rel):
    """Thinnest material in a plate half: min distance between any two outline rings."""
    m = load(rel)
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
    return best, len(polys), sum(len(p.interiors) for p in polys)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("-D", action="append", default=[])
    args = ap.parse_args()
    I = info(args.D)
    print(f"case {I['case_w']:.1f} x {I['case_d']:.1f} mm, front {I['front_h']:.1f} mm, back {I['back_h']:.1f} mm, "
          f"typing angle {I['th']} deg\n")

    # ------------------------------------------------------------- printable parts
    printable = ["bottom_left", "bottom_right", "bottom_left_udb", "gaskets_tpu", "foam_left_tpu", "foam_right_tpu",
                 "test_switch", "test_joint", "test_gasket"]
    printable += [f"top_{s}_{side}" for s in ("open", "wkl", "hhkb") for side in ("left", "right")]
    printable += [f"plate_{l}_{side}" for l in LAYOUTS for side in ("left", "right")]
    for n in printable:
        m = load(f"stl/{n}.stl")
        ext = np.ptp(m.bounds, axis=0)
        lowest = m.vertices[:, 2].min()
        down = (m.face_normals[:, 2] < -0.999) & (np.abs(m.triangles_center[:, 2] - lowest) < 1e-3)
        ok = (m.is_watertight and m.is_volume and man(m).status() == m3d.Error.NoError
              and max(ext) <= BED and abs(lowest) < 1e-3 and m.area_faces[down].sum() > 100)
        check(f"{n}: valid solid, flat on the bed, fits 256 mm", ok,
              f"{ext[0]:.0f} x {ext[1]:.0f} x {ext[2]:.1f} mm, {m.volume / 1000:.0f} cm3, "
              f"first layer {m.area_faces[down].sum() / 100:.0f} cm2")

    # ---------------------------------------------------------------- the case
    BL, BR, BU = M("stl/bottom_left.stl"), M("stl/bottom_right.stl"), M("stl/bottom_left_udb.stl")
    bottom = BL + BR
    tops = {s: (M(f"preview/view_top_{s}_left.stl"), M(f"preview/view_top_{s}_right.stl")) for s in ("open", "wkl", "hhkb")}
    pcb = M("preview/view_pcb.stl")
    gaskets = M("preview/view_gaskets.stl")

    check("bottom halves do not collide", overlap(BL, BR) < TOL, f"{overlap(BL, BR):.3f} mm3")
    lift = along(I, dw=0.01)       # the top case sits on the bottom case; ignore face contact
    for s, (TL, TR) in tops.items():
        o = [overlap(TL, TR), overlap((TL + TR).translate(lift), bottom), overlap((TL + TR).translate(lift), BU + BR)]
        check(f"top case ({s}): halves and bottom case meet without overlapping", max(o) < TOL,
              ", ".join(f"{x:.3f}" for x in o) + " mm3")
    top = tops["open"][0] + tops["open"][1]
    case = bottom + top

    # Top case really sits on the bottom case: drop it 0.05 mm and it must touch
    drop = overlap(top.translate(along(I, dw=-0.05)), bottom)
    check("top case rests on the bottom case at the parting plane", drop > 1, f"{drop:.1f} mm3 at 0.05 mm drop")

    free = {"+0.1 Y": (0, 0.1, 0), "-0.1 Y": (0, -0.1, 0), "+0.1 Z": (0, 0, 0.1), "-0.1 X": (-0.1, 0, 0)}
    slack = {k: overlap(BL.translate(list(d)), BR) for k, d in free.items()}
    check("seam T-keys: 0.1 mm of slack in every direction", all(v < TOL for v in slack.values()))
    locked = {k: overlap(BL.translate(list(d)), BR) for k, d in
              {"pull 0.2 X": (-0.2, 0, 0), "+0.2 Y": (0, 0.2, 0), "-0.2 Y": (0, -0.2, 0)}.items()}
    check("seam T-keys lock the bottom halves (seam opens < 0.2 mm)", all(v > 1 for v in locked.values()),
          ", ".join(f"{k}: {v:.1f} mm3" for k, v in locked.items()))

    # ------------------------------------------------------ gaskets and floating stack
    check("PCB floats: clears the case (and daughterboard) everywhere", overlap(pcb, case) < TOL
          and overlap(pcb, BU + BR + M("preview/view_udb.stl")) < TOL)
    down1 = pcb.translate(along(I, dw=-1.0))
    check("PCB can flex 1 mm down without touching the floor or daughterboard",
          overlap(down1, bottom) < TOL and overlap(down1, M("preview/view_udb.stl")) < TOL)
    sq = I["gasket_t"] - I["gasket_c"]
    check("gasket squeeze", abs((I["w_plate_bot"] - I["w_seat"]) - I["gasket_c"]) < 1e-6
          and abs((I["w_ceiling"] - I["w_plate_top"]) - I["gasket_c"]) < 1e-6 and 0.2 <= sq <= 0.6,
          f"{I['gasket_t']} mm pads compressed to {I['gasket_c']} mm above and below every tab "
          f"({sq / I['gasket_t'] * 100:.0f}%)")
    check("gasket pads sit inside their pockets (touch, no overlap)", overlap(gaskets, case) < TOL)
    foam = M("preview/view_foam.stl").translate(along(I, dw=0.01))
    check("case foam clears the case and leaves room for hot-swap sockets",
          overlap(foam, case) < TOL and I["w_pcb_bot"] - I["foam_t"] >= 1.9,
          f"{I['w_pcb_bot'] - I['foam_t']:.1f} mm above {I['foam_t']} mm foam")

    # ------------------------------------------------------------------ screws
    ok_all, notes = True, []
    for i, (x, y, zt, zs) in enumerate(I["screws"], 1):
        head = cyl_z(x, y, zs - 3.0, zs - 0.05, 5.5)                 # M3 socket head (5.5 x 3)
        shank = cyl_z(x, y, zs + 0.35, zs + 10 - 0.05, 2.9)           # above the membrane, 10 mm screw
        insert = cyl_z(x, y, zt - 0.3, zt + 4.0, 3.9)                 # insert body in the top case
        o = [overlap(head, bottom), overlap(shank, bottom), overlap(shank, top), overlap(insert, top)]
        grip_bottom, into_top = zt - zs, zs + 10 - zt
        good = max(o) < TOL and 3.0 <= into_top <= 5.0
        ok_all &= good
        notes.append(f"#{i}: {grip_bottom:.1f} mm clamp + {into_top:.1f} mm in insert")
    check("8 x M3 x 10: head seats in its counterbore, shank reaches the insert", ok_all, "; ".join(notes[:2]) + " ...")

    # --------------------------------------------------------------- USB paths
    ua, ub = I["usb"]
    u_off, v_edge = I["u_p"], I["v_p"] + I["pcb_depth"]
    fails, centers = [], np.array(sorted(set(np.arange(ua + 7, ub - 7 + 1e-6, 1.0)) | {18.2}))
    for c in centers:
        for wc in (I["w_pcb_bot"] - 1.6, I["w_pcb_bot"] + 0.8):          # bottom-mount, mid-mount receptacle
            for flex in (0.0, -1.0):
                plug = box_t(I, u_off + c - 6.2, u_off + c + 6.2, v_edge + 0.3, v_edge + 40,
                             wc + flex - 3.25, wc + flex + 3.25)
                if overlap(plug, case) > TOL:
                    fails.append((round(float(c), 1), wc, flex))
    check("USB-C plug (12.4 x 6.5 mm) reaches a PCB-mounted port", not fails,
          f"port centres {centers[0]:.0f}-{centers[-1]:.0f} mm from the PCB's left edge (GH60 standard: 18.2), "
          "bottom- or mid-mount, PCB at rest and flexed 1 mm" + (f"; blocked: {fails[:3]}" if fails else ""))
    uc, uw, uv1 = I["udb"]
    plug = box_t(I, uc - 6.2, uc + 6.2, uv1, uv1 + 40, uw - 3.25, uw + 3.25)
    check("USB-C plug reaches the daughterboard port (udb version)", overlap(plug, BU + BR + top) < TOL
          and overlap(M("preview/view_udb.stl"), BU + BR + top) < TOL)

    # ------------------------------------------------------- per-layout checks
    for lay, style in LAYOUTS.items():
        TL, TR = tops[style]
        case_l = bottom + TL + TR
        PL, PR = M(f"preview/view_plate_{lay}_left.stl"), M(f"preview/view_plate_{lay}_right.stl")
        plate = PL + PR
        caps, pressed = M(f"preview/view_keycaps_{lay}.stl"), M(f"preview/view_keycaps_{lay}_pressed.stl")
        sw = M(f"preview/view_switches_{lay}.stl")
        o = {"plate": overlap(plate, case_l), "switches": overlap(sw, case_l), "keycaps": overlap(caps, case_l),
             "pressed": overlap(pressed, case_l), "halves": overlap(PL, PR), "pcb": overlap(plate, pcb)}
        check(f"{lay}: plate, switches and keycaps (at rest and pressed) clear the case", max(o.values()) < TOL,
              ", ".join(f"{k} {v:.3f}" for k, v in o.items()))
        c = I["tab_clearance"] - 0.05
        worst = max(overlap(pressed.translate(along(I, du=du, dv=dv)), TL + TR)
                    for du, dv in ((c, 0), (-c, 0), (0, c), (0, -c)))
        check(f"{lay}: pressed keycaps clear walls{' and blockers' if style != 'open' else ''} "
              f"with the plate floated {I['tab_clearance'] - 0.05:.2f} mm sideways", worst < TOL, f"{worst:.3f} mm3")
        lift = overlap(plate.translate(along(I, dw=0.9)), TL + TR)
        sink = overlap(plate.translate(along(I, dw=-I['gasket_c'] + 0.05)), bottom)
        check(f"{lay}: plate can sink {I['gasket_c'] - 0.05:.2f} mm into its gaskets; "
              f"{'top case and blockers stay' if style != 'open' else 'top case stays'} 0.9 mm clear above it",
              lift < TOL and sink < TOL, f"{sink:.3f} / {lift:.3f} mm3")
        for side, P in (("left", f"stl/plate_{lay}_left.stl"), ("right", f"stl/plate_{lay}_right.stl")):
            (d, where), npoly, holes = plate_web(P)
            check(f"{lay}: plate {side} half is one piece, thinnest web >= 1.0 mm", d >= 1.0 and npoly == 1,
                  f"{d:.2f} mm near {where}, {holes} cutouts")

    failed = [r for r in results if not r[1]]
    print(f"\n{len(results) - len(failed)}/{len(results)} checks passed")
    sys.exit(1 if failed else 0)


if __name__ == "__main__":
    main()
