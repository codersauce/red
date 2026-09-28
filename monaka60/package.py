#!/usr/bin/env python3
"""Lay the printable STLs out on 256 x 256 mm beds and write one 3MF per print job.

    python3 package.py      # after build.py

Writes print/*.3mf (open them in Bambu Studio), print/layout.json and a
filament estimate. Needs numpy + trimesh.
"""
import json
import os
import zipfile

import numpy as np
import trimesh

HERE = os.path.dirname(os.path.abspath(__file__))
BED = 256.0
GAP = 10.0

JOBS = {
    "monaka60_0_fit_tests": {"parts": ["test_switch", "test_joint", "test_gasket"], "stack": "x"},
    "monaka60_1_bottom": {"parts": ["bottom_left", "bottom_right"], "stack": "y"},
    "monaka60_1_bottom_udb": {"parts": ["bottom_left_udb", "bottom_right"], "stack": "y"},
    "monaka60_2_top_open": {"parts": ["top_open_left", "top_open_right"], "stack": "y"},
    "monaka60_2_top_wkl": {"parts": ["top_wkl_left", "top_wkl_right"], "stack": "y"},
    "monaka60_2_top_hhkb": {"parts": ["top_hhkb_left", "top_hhkb_right"], "stack": "y"},
    "monaka60_3_plate_ansi": {"parts": ["plate_ansi_left", "plate_ansi_right"], "stack": "y"},
    "monaka60_3_plate_tsangan": {"parts": ["plate_tsangan_left", "plate_tsangan_right"], "stack": "y"},
    "monaka60_3_plate_wkl": {"parts": ["plate_wkl_left", "plate_wkl_right"], "stack": "y"},
    "monaka60_3_plate_hhkb": {"parts": ["plate_hhkb_left", "plate_hhkb_right"], "stack": "y"},
    "monaka60_4_gaskets_tpu": {"parts": ["gaskets_tpu"], "stack": "x"},
    "monaka60_5_foam_tpu": {"parts": ["foam_left_tpu", "foam_right_tpu"], "stack": "y"},
}

DENSITY = {"PLA": 1.24, "PETG": 1.27}   # g/cm3


def arrange(meshes, stack):
    """Centre the parts on the bed, stacked along X or Y with GAP between them."""
    ext = [np.ptp(m.bounds, axis=0) for m in meshes]
    axis = 0 if stack == "x" else 1
    total = sum(e[axis] for e in ext) + GAP * (len(meshes) - 1)
    cursor = (BED - total) / 2
    placed = []
    for m, e in zip(meshes, ext):
        pos = [0.0, 0.0]
        pos[axis] = cursor + e[axis] / 2
        pos[1 - axis] = BED / 2
        cursor += e[axis] + GAP
        placed.append(pos)
        lo = [pos[0] - e[0] / 2, pos[1] - e[1] / 2]
        assert lo[0] >= 0 and lo[1] >= 0 and lo[0] + e[0] <= BED and lo[1] + e[1] <= BED, "does not fit the bed"
    return placed


def write_3mf(path, named_meshes, title):
    """Minimal 3MF core-spec writer: one object per part, placed by its build transform."""
    objs, items = [], []
    for i, (name, mesh, (x, y)) in enumerate(named_meshes, 1):
        c = (mesh.bounds[0] + mesh.bounds[1]) / 2
        v = mesh.vertices - [c[0], c[1], mesh.bounds[0][2]]      # object origin: centre of its footprint
        verts = "".join(f'<vertex x="{a:.4f}" y="{b:.4f}" z="{d:.4f}"/>' for a, b, d in v)
        tris = "".join(f'<triangle v1="{a}" v2="{b}" v3="{d}"/>' for a, b, d in mesh.faces)
        objs.append(f'<object id="{i}" name="{name}" type="model"><mesh><vertices>{verts}</vertices>'
                    f'<triangles>{tris}</triangles></mesh></object>')
        items.append(f'<item objectid="{i}" transform="1 0 0 0 1 0 0 0 1 {x:.3f} {y:.3f} 0"/>')
    model = ('<?xml version="1.0" encoding="UTF-8"?>\n'
             '<model unit="millimeter" xml:lang="en-US" '
             'xmlns="http://schemas.microsoft.com/3dmanufacturing/core/2015/02">'
             f'<metadata name="Title">{title}</metadata>'
             '<metadata name="Designer">Monaka60</metadata>'
             f'<resources>{"".join(objs)}</resources><build>{"".join(items)}</build></model>')
    types = ('<?xml version="1.0" encoding="UTF-8"?>\n'
             '<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">'
             '<Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/>'
             '<Default Extension="model" ContentType="application/vnd.ms-package.3dmanufacturing-3dmodel+xml"/>'
             '</Types>')
    rels = ('<?xml version="1.0" encoding="UTF-8"?>\n'
            '<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">'
            '<Relationship Target="/3D/3dmodel.model" Id="rel0" '
            'Type="http://schemas.microsoft.com/3dmanufacturing/2013/01/3dmodel"/></Relationships>')
    with zipfile.ZipFile(path, "w", zipfile.ZIP_DEFLATED) as z:
        z.writestr("[Content_Types].xml", types)
        z.writestr("_rels/.rels", rels)
        z.writestr("3D/3dmodel.model", model)


def filament_grams(mesh, walls_mm, infill):
    """Rough slicer-style estimate in grams of PLA: a solid shell of walls_mm plus sparse infill."""
    v = mesh.volume / 1000.0                          # cm3
    shell_v = min(v, mesh.area * walls_mm / 1000.0)
    return round(DENSITY["PLA"] * (shell_v + (v - shell_v) * infill))


# Estimates for two slicer setups: Bambu's "0.20mm Standard" defaults (2 walls, 15% infill)
# and the heavier setup recommended for the case (4 walls, 40% gyroid).
SETUPS = {"default": (0.9, 0.15), "heavy": (1.7, 0.40)}


def main():
    os.makedirs(os.path.join(HERE, "print"), exist_ok=True)
    layout, stats = {}, {}
    for job, spec in JOBS.items():
        meshes = [trimesh.load(os.path.join(HERE, "stl", f"{p}.stl"), process=True) for p in spec["parts"]]
        placed = arrange(meshes, spec["stack"])
        write_3mf(os.path.join(HERE, "print", f"{job}.3mf"),
                  list(zip(spec["parts"], meshes, placed)), job)
        layout[job] = [{"part": p, "center": xy,
                        "footprint_center": ((m.bounds[0][:2] + m.bounds[1][:2]) / 2).tolist(),
                        "size": np.ptp(m.bounds, axis=0).round(2).tolist()}
                       for p, m, xy in zip(spec["parts"], meshes, placed)]
        stats[job] = {p: {"solid_cm3": round(m.volume / 1000, 1),
                          **{k: filament_grams(m, *su) for k, su in SETUPS.items()}}
                      for p, m in zip(spec["parts"], meshes)}
        print(f"{job}.3mf: " + ", ".join(f"{p} {np.ptp(m.bounds, axis=0)[0]:.0f}x{np.ptp(m.bounds, axis=0)[1]:.0f} mm"
                                          for p, m in zip(spec["parts"], meshes)))
    with open(os.path.join(HERE, "print", "layout.json"), "w") as f:
        json.dump({"bed": BED, "jobs": layout, "filament_g": stats}, f, indent=1)
    print("filament estimate, grams of PLA (PETG is ~2% heavier):")
    for job, parts in stats.items():
        for p, st in parts.items():
            print(f"  {p:22s} solid {st['solid_cm3']:6.1f} cm3   default {st['default']:4d} g   heavy {st['heavy']:4d} g")


if __name__ == "__main__":
    main()
