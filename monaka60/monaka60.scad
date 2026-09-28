// =====================================================================
//  MONAKA60: a 3D-printable, gasket-mounted 60% keyboard
//
//  Two crisp printed shells with a soft middle: the switch plate floats on
//  gasket pads clamped between the top and bottom case.
//
//  * Fits a 256 x 256 mm bed (Bambu Lab X1C / X1E / P1S / P1P). Case and
//    plate split into halves; every part prints flat with no supports.
//  * Gasket mount: 12 plate tabs sit between 2 mm gasket pads (printed TPU
//    or Poron), clamped by 8 hidden M3 screws driven from underneath.
//  * Takes any standard 285 x 94.6 mm GH60-style PCB (hineybush h60, DZ60,
//    BM60, ...). The PCB hangs from the switches, so its mounting holes are
//    not used. USB-C on the PCB, or a Unified Daughterboard (C3/C4) for JST
//    boards such as the h60 hot-swap.
//  * Layouts: ANSI, Tsangan, WKL and HHKB. The top case carries the WKL or
//    HHKB blockers.
//
//  Coordinates: X = left -> right, Y = front (user side) -> back, Z = up.
//  The PCB/plate stack lives in a frame tilted about X by typing_angle:
//  u = X, v = along the plate toward the back, w = up from the case floor.
//
//  Export one part at a time, e.g.
//    openscad -o top_left.stl -D 'part="top_left"' -D 'layout="hhkb"' monaka60.scad
//  or run build.py to export and verify everything.
// =====================================================================

/* [What to render] */
// Printable parts come out in print orientation; view_* parts are assembled
part = "assembly"; // [assembly, bottom_left, bottom_right, top_left, top_right, plate_left, plate_right, plate_2d, gaskets, gasket_2d, foam_left, foam_right, foam_2d, test_switch, test_joint, test_gasket, view_top_left, view_top_right, view_plate_left, view_plate_right, view_pcb, view_switches, view_keycaps, view_keycaps_pressed, view_gaskets, view_udb, view_foam, info]

/* [Layout] */
layout = "ansi"; // [ansi:ANSI (6.25u space), tsangan:Tsangan (7u), wkl:WKL (7u with blockers), hhkb:HHKB (7u with blockers)]
// Blockers in the top case; auto follows the layout
blockers = "auto"; // [auto, none, wkl, hhkb]

/* [USB connection] */
// usb = USB-C on the PCB (wide slot); udb = JST PCB + Unified Daughterboard C3/C4
connector = "usb"; // [usb, udb]
// usb: slot extent in mm from the PCB's left edge (the GH60 port centre is 18.2)
usb_slot_from = 8;
usb_slot_to = 58;
// udb: daughterboard port centre in mm from the PCB's left edge
udb_port_x = 18.2;

/* [Case shape] */
// Plate / typing angle in degrees
typing_angle = 6; // [0:0.5:10]
bezel_side = 9;
bezel_front = 9;
bezel_back = 11;
// Floor thickness at its thinnest (front) point
floor_min = 5;
// Rim height above the plate: 6 mm hides the switch housings and gives blockers depth
rim_above_plate = 6;
corner_radius = 4;
top_chamfer = 1.0;
// 45 degree chamfer on the bottom edge; hides elephant's foot
bottom_chamfer = 0.8;
inner_chamfer = 0.6;
// Shadow line where the top and bottom case meet
parting_chamfer = 0.6;

/* [PCB and plate] */
pcb_width = 285;
pcb_depth = 94.6;
pcb_thickness = 1.6;
// Gap between the floating PCB/plate edge and the case wall
edge_clearance = 1.0;
// Floor to PCB underside at rest: room for hot-swap sockets, flex and foam
pcb_floor_gap = 4;
plate_thickness = 1.5;
// MX cutout, 14.0 = spec. Fine-tune fit with Bambu Studio "X-Y hole compensation"
switch_cutout = 14.0;
// pcb = roomy cutouts for PCB-mount stabilizers; cherry = exact Cherry spec (plate-mount)
stab_style = "pcb"; // [pcb, cherry]
plate_split_gap = 0.2;

/* [Gasket mount] */
gasket_thickness = 2.0;
// How much each gasket is compressed once the case is screwed shut
gasket_squeeze = 0.35;
tab_width = 12;
// How far each tab reaches past the plate edge
tab_length = 5.5;
// Clearance around each tab in its pocket (also how far the plate can float sideways)
tab_clearance = 0.4;
// Blocker faces sit this far inside their key unit (keycap gap = this + 0.475)
blocker_gap = 0.6;
// M3 socket head screws from below into M3 heat-set inserts in the top case
screw_length = 10;
insert_engagement = 4;
insert_hole_diameter = 4.0;
insert_hole_depth = 5.0;
screw_clearance_diameter = 3.4;
counterbore_diameter = 5.8;
// Sacrificial layer printed over each counterbore; pierce it with the screw
membrane = 0.3;

/* [Split and joint] */
// Seam position relative to the case centre (mm, + moves it right)
split_offset = 0;
key_neck_width = 8;
key_neck_length = 4;
key_head_width = 16;
key_head_length = 5;
// Clearance on every face of the T-key slots
key_clearance = 0.15;
key_roof = 1.2;
// Decorative V-groove along the seam (0 = off)
seam_groove = 0.6;

/* [Feet] */
feet = true;
foot_diameter = 12.5;
foot_depth = 0.6;
foot_inset = 18;

/* [Case foam (optional)] */
foam_thickness = 2;

/* [Hidden] */
$fn = 48;
eps = 0.01;
U = 19.05;
th = typing_angle;

cav_w = pcb_width + 2 * edge_clearance;
cav_d = pcb_depth + 2 * edge_clearance;
cav_r = 1.0;
case_w = cav_w + 2 * bezel_side;

// Stack heights in the tilted frame (w = 0 is the case floor)
w_pcb_bot = pcb_floor_gap;
w_pcb_top = w_pcb_bot + pcb_thickness;
w_plate_top = w_pcb_top + 5.0;                  // MX: plate top 5.0 mm above the PCB top
w_plate_bot = w_plate_top - plate_thickness;
w_rim = w_plate_top + rim_above_plate;
w_part = w_plate_bot;                           // parting plane between bottom and top case
g_c = gasket_thickness - gasket_squeeze;        // compressed gasket
w_seat = w_plate_bot - g_c;                     // bottom gasket seat (bottom case)
w_ceiling = w_plate_top + g_c;                  // top gasket ceiling (top case)
w_blocker = w_plate_top + 1.0;                  // blocker underside

Y0 = bezel_front + w_rim * sin(th);
Z0 = floor_min;
case_d = bezel_front + cav_d * cos(th) + bezel_back;
split_x = case_w / 2 + split_offset;
u_p = bezel_side + edge_clearance;              // PCB / plate left edge
v_p = edge_clearance;                           // PCB / plate front edge

function floor_z(y) = Z0 + (y - Y0) * tan(th);
function rim_z(y) = Z0 + (y - Y0) * tan(th) + w_rim / cos(th);
function T2W(u, v, w) = [u, Y0 + v * cos(th) - w * sin(th), Z0 + v * sin(th) + w * cos(th)];

module tilted() translate([0, Y0, Z0]) rotate([th, 0, 0]) children();

module rrect(x, y, w, d, r) {
    translate([x, y])
        if (r > 0) offset(r = r) offset(delta = -r) square([w, d]);
        else square([w, d]);
}

// ---------------------------------------------------------------------
//  Layouts (rows of key widths; negative = empty / blocker)
// ---------------------------------------------------------------------
rows_top_ansi = [
    [1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 2],
    [1.5, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1.5],
    [1.75, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 2.25],
    [2.25, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 2.75]];
rows_top_hhkb = [
    [1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1],
    [1.5, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1.5],
    [1.75, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 2.25],
    [2.25, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1.75, 1]];
row4 = layout == "ansi" ? [1.25, 1.25, 1.25, 6.25, 1.25, 1.25, 1.25, 1.25]
     : layout == "wkl"  ? [1.5, -1, 1.5, 7, 1.5, -1, 1.5]
     : layout == "hhkb" ? [-1.5, 1, 1.5, 7, 1.5, 1, -1.5]
     :                    [1.5, 1, 1.5, 7, 1.5, 1, 1.5];
rows = concat(layout == "hhkb" ? rows_top_hhkb : rows_top_ansi, [row4]);

// Plate seam per row, in key units. Each value is a key boundary, or open plate
// beside the spacebar, so no cutout is cut and no tab is split.
seam_units = layout == "ansi" ? [8, 7.5, 7.75, 7.25, 7.5] : [8, 7.5, 7.75, 7.25, 6];

function cumsum(v, i) = i <= 0 ? 0 : cumsum(v, i - 1) + abs(v[i - 1]);
function row_keys(r, y) = [for (i = [0 : len(r) - 1]) if (r[i] > 0) [cumsum(r, i), y, r[i]]];
keys = [for (y = [0 : len(rows) - 1]) each row_keys(rows[y], y)];   // [x_u, y_u, width_u]

kx0 = (15 * U - pcb_width) / 2;                 // the key grid is centred on the PCB
ky0 = (5 * U - pcb_depth) / 2;
function ku(kx) = u_p + kx - kx0;               // key-grid mm -> u
function kv(ky) = v_p + pcb_depth - (ky - ky0); // key-grid mm (down) -> v
function key_center(k) = [ku((k[0] + k[2] / 2) * U), kv((k[1] + 0.5) * U)];

blocker_style = blockers != "auto" ? blockers
              : layout == "wkl" ? "wkl" : layout == "hhkb" ? "hhkb" : "none";
blocker_keys = blocker_style == "hhkb" ? [[0, 4, 1.5], [13.5, 4, 1.5]]
             : blocker_style == "wkl"  ? [[1.5, 4, 1], [12.5, 4, 1]] : [];

// Stabilizer spacing from the switch centre, by key width
function stab_offset(w) = w >= 8 ? 66.675 : w >= 7 ? 57.15 : w >= 6.25 ? 50 : w >= 6 ? 47.625
                        : w >= 3 ? 19.05 : w >= 2 ? 11.938 : 0;
stab_w     = stab_style == "cherry" ? 6.65 : 7.0;
stab_back  = stab_style == "cherry" ? 5.53 : 5.73;
stab_front = stab_style == "cherry" ? 6.77 : 6.97;

// ---------------------------------------------------------------------
//  Gasket tabs, screws and pegs
// ---------------------------------------------------------------------
// [edge, position in key units along that edge]. Chosen to clear the plate
// seams (back 8u; front 7.5u ANSI / 6u 7u-family), the case seam and the screws.
tabs = [
    ["back", 4], ["back", 6.25], ["back", 9.25], ["back", 12], ["back", 14],
    ["front", 3], ["front", 5], ["front", 8.75], ["front", 11.75], ["front", 13.75],
    ["left", 2.5], ["right", 2.5]];

// [u, v, width, depth] of a tab, including 1 mm of overlap into the plate
function tab_rect(t) =
    t[0] == "back"  ? [ku(t[1] * U) - tab_width / 2, v_p + pcb_depth - 1, tab_width, tab_length + 1] :
    t[0] == "front" ? [ku(t[1] * U) - tab_width / 2, v_p - tab_length, tab_width, tab_length + 1] :
    t[0] == "left"  ? [u_p - tab_length, kv(t[1] * U) - tab_width / 2, tab_length + 1, tab_width] :
                      [u_p + pcb_width - 1, kv(t[1] * U) - tab_width / 2, tab_length + 1, tab_width];
module tab_2d(t) { r = tab_rect(t); rrect(r[0], r[1], r[2], r[3], 1.5); }
module pocket_2d(t) {
    r = tab_rect(t);
    translate([r[0] - tab_clearance, r[1] - tab_clearance]) square([r[2] + 2 * tab_clearance, r[3] + 2 * tab_clearance]);
}
// The part of a pocket inside the wall, where a gasket pad sits
module gasket_2d(t) offset(delta = -0.25) difference() { pocket_2d(t); cavity_2d(); }
pad_w = tab_width + 2 * tab_clearance - 0.5;
pad_d = tab_length + tab_clearance - edge_clearance - 0.5;

// Wall centre lines at the parting plane
v_front_outer = (-Y0 + w_part * sin(th)) / cos(th);
v_back_outer = (case_d - Y0 + w_part * sin(th)) / cos(th);
v_fm = v_front_outer / 2;
v_bm = (cav_d + v_back_outer) / 2;

// Screw positions (u, v) at the parting plane: four corners plus both sides of the seam
screws = [[6, v_fm + 1], [6, v_bm], [case_w - 6, v_fm + 1], [case_w - 6, v_bm],
          [split_x - 9.5, v_fm], [split_x + 9.5, v_fm], [split_x - 9.5, v_bm], [split_x + 9.5, v_bm]];
function screw_top(s) = T2W(s[0], s[1], w_part);                   // where the screw meets the top case
function screw_seat(s) = screw_top(s)[2] - (screw_length - insert_engagement);

// Alignment pegs (u, v) on the bottom case's top face
pegs = [[ku(76), v_fm], [ku(97), v_bm], [ku(195), v_fm], [ku(202), v_bm]];

// ---------------------------------------------------------------------
//  Plate (2D, u/v frame, seen from above)
// ---------------------------------------------------------------------
module plate_outline() offset(r = -0.8) offset(delta = 0.8) union() {
    rrect(u_p, v_p, pcb_width, pcb_depth, 1);
    for (t = tabs) tab_2d(t);
}

module stab_cutouts(offset_mm) {
    for (sx = [-offset_mm, offset_mm])
        translate([sx - stab_w / 2, -stab_front]) square([stab_w, stab_front + stab_back]);
}

module key_cutouts(k) {
    translate(key_center(k)) {
        square([switch_cutout, switch_cutout], center = true);
        if (stab_offset(k[2]) > 0) stab_cutouts(stab_offset(k[2]));
    }
}

module plate_2d() difference() {
    plate_outline();
    for (k = keys) key_cutouts(k);
}

module plate_left_region() {
    for (r = [0 : 4]) {
        v_top = r == 0 ? 500 : kv(r * U);
        v_bot = r == 4 ? -500 : kv((r + 1) * U);
        translate([-500, v_bot]) square([ku(seam_units[r] * U) + 500, v_top - v_bot]);
    }
}
module plate_left_2d() intersection() { plate_2d(); offset(delta = -plate_split_gap / 2) plate_left_region(); }
module plate_right_2d() difference() { plate_2d(); offset(delta = plate_split_gap / 2) plate_left_region(); }

// ---------------------------------------------------------------------
//  Case shells
// ---------------------------------------------------------------------
module cavity_2d() rrect(bezel_side, 0, cav_w, cav_d, cav_r);

module blocker_2d(b) {
    x0 = b[0] * U;
    x1 = (b[0] + b[2]) * U;
    y0 = b[1] * U;
    y1 = (b[1] + 1) * U;
    ul = x0 <= 0.01 ? -20 : ku(x0) + blocker_gap;                 // edges on the key grid's
    ur = x1 >= 15 * U - 0.01 ? case_w + 20 : ku(x1) - blocker_gap; // border run into the wall
    vb = y0 <= 0.01 ? cav_d + 20 : kv(y0) - blocker_gap;
    vf = y1 >= 5 * U - 0.01 ? -20 : kv(y1) + blocker_gap;
    translate([ul, vf]) square([ur - ul, vb - vf]);
}

// Top opening: the cavity minus the blockers, every corner rounded
module opening_2d() offset(r = -1) offset(delta = 1) offset(r = 1) offset(delta = -1)
    difference() { cavity_2d(); for (b = blocker_keys) blocker_2d(b); }

module slab_world(inset, z)
    translate([0, 0, z]) linear_extrude(eps)
        rrect(inset, inset, case_w - 2 * inset, case_d - 2 * inset, corner_radius - inset);

// Thin slab in the tilted frame between w0 and w1 whose top-view footprint is
// the case outline shrunk by `inset`
module slab_at(inset, w0, w1) {
    tilted() translate([0, 0, w0]) linear_extrude(w1 - w0)
        translate([0, (w1 * sin(th) - Y0) / cos(th)]) scale([1, 1 / cos(th)])
            rrect(inset, inset, case_w - 2 * inset, case_d - 2 * inset, corner_radius - inset);
}

module bottom_outer() hull() {
    slab_world(bottom_chamfer, 0);
    slab_world(0, bottom_chamfer);
    slab_at(0, w_part - parting_chamfer - eps, w_part - parting_chamfer);
    slab_at(parting_chamfer, w_part - eps, w_part);
}

module top_outer() hull() {
    slab_at(parting_chamfer, w_part, w_part + eps);
    slab_at(0, w_part + parting_chamfer, w_part + parting_chamfer + eps);
    slab_at(0, w_rim - top_chamfer - eps, w_rim - top_chamfer);
    slab_at(top_chamfer, w_rim - eps, w_rim);
}

module rim_chamfer_cutter() translate([0, 0, w_rim - inner_chamfer]) minkowski() {
    linear_extrude(eps) opening_2d();
    cylinder(r1 = 0, r2 = inner_chamfer + 2, h = inner_chamfer + 2, $fn = 16);
}

module usb_notch() tilted() translate([0, cav_d + 40, 0]) rotate([90, 0, 0]) linear_extrude(42)
    rrect(u_p + usb_slot_from, -2.4, usb_slot_to - usb_slot_from, w_part + 5.4, 1.5);

// ---------------------------------------------------------------------
//  Unified Daughterboard C3/C4 (18 x 16.5 mm, 4 x M2 on 14 x 12.5 mm)
// ---------------------------------------------------------------------
udb_w = 18;
udb_d = 16.5;
udb_gap = 0.5;                                   // port edge to back wall
udb_bottom = -2.5;                               // board underside (w)
udb_u0 = u_p + udb_port_x - udb_w / 2;
udb_v1 = cav_d - udb_gap;                        // port edge
udb_holes = [[2, 2], [16, 2], [2, 14.5], [16, 14.5]];   // [x, y from the port edge]
udb_port_w = udb_bottom + 1.6 + 1.63;            // USB-C centre on a top-mount receptacle

module udb_cuts() tilted() {
    translate([udb_u0 - 0.5, udb_v1 - udb_d - 0.5, udb_bottom - 1]) cube([udb_w + 1, udb_d + 1 + udb_gap + 1, 10]);
    translate([0, cav_d + 40, 0]) rotate([90, 0, 0]) linear_extrude(42 + 2)
        rrect(u_p + udb_port_x - 7, udb_port_w - 4.9, 14, 9.8, 1.5);
}
module udb_bosses() tilted() for (h = udb_holes)
    translate([udb_u0 + h[0], udb_v1 - h[1], udb_bottom - 1 - eps]) cylinder(d = 4.6, h = 1 + eps);
module udb_screw_holes() tilted() for (h = udb_holes)
    translate([udb_u0 + h[0], udb_v1 - h[1], udb_bottom - 5]) cylinder(d = 1.8, h = 6);

// ---------------------------------------------------------------------
//  Screws, pegs, feet, seam
// ---------------------------------------------------------------------
module screw_holes_bottom() for (s = screws) {
    p = screw_top(s);
    z_seat = screw_seat(s);
    translate([p[0], p[1], -1]) cylinder(d = counterbore_diameter, h = z_seat + 1);
    translate([p[0], p[1], z_seat + membrane]) cylinder(d = screw_clearance_diameter, h = p[2] - z_seat + 5);
}

module insert_holes() for (s = screws) {
    p = screw_top(s);
    translate([p[0], p[1], p[2] - 2]) cylinder(d = insert_hole_diameter, h = insert_hole_depth + 2);
    translate([p[0], p[1], p[2] - 2]) cylinder(d = insert_hole_diameter + 0.8, h = 2 + 0.4);   // lead-in
}

module peg_shapes(left) tilted() for (g = pegs) if ((g[0] < split_x) == left)
    translate([g[0], g[1], w_part - eps]) {
        cylinder(d = 3.0, h = 1.6 + eps);
        translate([0, 0, 1.6]) cylinder(d1 = 3.0, d2 = 2.2, h = 0.4);
    }
module peg_holes() tilted() for (g = pegs)
    translate([g[0], g[1], w_part - 1]) cylinder(d = 3.4, h = 1 + 2.5);

module feet_recesses() if (feet)
    for (x = [foot_inset, case_w - foot_inset], y = [foot_inset, case_d - foot_inset])
        translate([x, y, -1]) cylinder(d = foot_diameter, h = 1 + foot_depth);

key_y = [for (f = [0.2, 0.5, 0.8]) Y0 + cav_d * cos(th) * f];
function key_top(yc) = floor_z(yc - key_head_width / 2 - key_clearance) - key_roof - 0.2;

module key_shape(x0, yc) {
    translate([x0 - 1, yc - key_neck_width / 2]) square([key_neck_length + 1 + eps, key_neck_width]);
    translate([x0 + key_neck_length, yc - key_head_width / 2]) square([key_head_length, key_head_width]);
}
module key_2d(x0, yc, c = 0) {
    if (c == 0) offset(r = 0.4) offset(delta = -0.4) key_shape(x0, yc);
    else offset(delta = c) key_shape(x0, yc);
}
module seam_keys() for (yc = key_y) linear_extrude(key_top(yc)) key_2d(split_x, yc);
module seam_key_slots() for (yc = key_y)
    translate([0, 0, -1]) linear_extrude(key_top(yc) + 0.2 + 1) key_2d(split_x, yc, key_clearance);

module groove_vertical() {
    s = seam_groove * sqrt(2);
    for (y = [0, case_d]) translate([split_x, y, -1]) linear_extrude(100) rotate(45) square(s, center = true);
}
module seam_grooves_bottom() if (seam_groove > 0) {
    groove_vertical();
    translate([split_x, -1, 0]) rotate([-90, 0, 0]) linear_extrude(case_d + 2)
        rotate(45) square(seam_groove * sqrt(2), center = true);
}
module seam_grooves_top() if (seam_groove > 0) {
    groove_vertical();
    tilted() translate([split_x, -40, w_rim]) rotate([-90, 0, 0]) linear_extrude(cav_d + 80)
        rotate(45) square(seam_groove * sqrt(2), center = true);
}

module half_box(left) {
    if (left) translate([-50, -50, -50]) cube([split_x + 50, case_d + 100, 200]);
    else translate([split_x, -50, -50]) cube([case_w - split_x + 50, case_d + 100, 200]);
}

// ---------------------------------------------------------------------
//  Bottom case: tub, gasket seats, USB, T-key seam, screws from below
// ---------------------------------------------------------------------
module bottom_solid() difference() {
    bottom_outer();
    tilted() {
        linear_extrude(w_part + 10) cavity_2d();
        for (t = tabs) translate([0, 0, w_seat]) linear_extrude(w_part - w_seat + 5) pocket_2d(t);
    }
    if (connector == "usb") usb_notch(); else udb_cuts();
    screw_holes_bottom();
    feet_recesses();
}

module bottom_case(left) difference() {
    union() {
        intersection() { bottom_solid(); half_box(left); }
        peg_shapes(left);
        if (left) seam_keys();
        if (left && connector == "udb") udb_bosses();
    }
    if (!left) seam_key_slots();
    if (left && connector == "udb") udb_screw_holes();
    seam_grooves_bottom();
}

// ---------------------------------------------------------------------
//  Top case: bezel frame with blockers, tab pockets and insert holes
// ---------------------------------------------------------------------
module top_solid() difference() {
    top_outer();
    tilted() {
        translate([0, 0, w_part - 1]) linear_extrude(w_blocker - w_part + 1 + eps) cavity_2d();
        translate([0, 0, w_blocker]) linear_extrude(w_rim - w_blocker + 5) opening_2d();
        rim_chamfer_cutter();
        for (t = tabs) translate([0, 0, w_part - 1]) linear_extrude(w_ceiling - w_part + 1) pocket_2d(t);
    }
    peg_holes();
    insert_holes();
}

module top_case(left) difference() {
    intersection() { top_solid(); half_box(left); }
    seam_grooves_top();
}

// Print orientation: flipped so the rim lies on the bed
module top_print(left) translate([0, 0, w_rim]) rotate([180, 0, 0]) rotate([-th, 0, 0]) translate([0, -Y0, -Z0])
    top_case(left);

// ---------------------------------------------------------------------
//  Gaskets and foam
// ---------------------------------------------------------------------
module gaskets_sheet() for (i = [0 : 2 * len(tabs) - 1])
    translate([(i % 6) * (pad_w + 3), floor(i / 6) * (pad_d + 3), 0]) cube([pad_w, pad_d, gasket_thickness]);

module foam_2d() difference() {
    offset(delta = -0.75) cavity_2d();
    translate([u_p + min(usb_slot_from, udb_port_x - 12) - 2, cav_d - 22]) square([60, 30]);
}
module foam_half(left) linear_extrude(foam_thickness) intersection() {
    foam_2d();
    if (left) translate([-50, -50]) square([split_x - 0.1 + 50, 300]);
    else translate([split_x + 0.1, -50]) square([300, 300]);
}

// ---------------------------------------------------------------------
//  Fit-test coupons (print these first)
// ---------------------------------------------------------------------
// Top row: 1u cutouts at switch_cutout -0.1 / +0 / +0.1 mm, marked with 1 / 2 / 3 dots.
// Bottom row: a 2u cutout with stabilizer cutouts.
module test_switch() {
    W = 3 * U + 6;
    H = 2 * U + 6;
    linear_extrude(plate_thickness) difference() {
        rrect(0, 0, W, H, 3);
        for (i = [0 : 2]) translate([3 + (i + 0.5) * U, 3 + 1.5 * U]) {
            square(switch_cutout - 0.1 + 0.1 * i, center = true);
            for (j = [0 : i]) translate([(j - i / 2) * 2.2, 9.6]) circle(d = 1.2);
        }
        translate([3 + 1.5 * U, 3 + 0.5 * U]) {
            square(switch_cutout, center = true);
            stab_cutouts(11.938);
        }
    }
}

// A 30 mm slice of the case seam: lower the right block onto the left one
module test_joint() {
    D = 30;
    yc = D / 2;
    h = floor_z(key_y[1] - key_head_width / 2 - key_clearance);
    kt = h - key_roof - 0.2;
    translate([-20, 0, 0]) cube([20, D, h]);
    linear_extrude(kt) key_2d(0, yc);
    translate([14, 0, 0]) difference() {
        cube([20, D, h]);
        translate([0, 0, -1]) linear_extrude(kt + 0.2 + 1) key_2d(0, yc, key_clearance);
    }
}

// One gasket stack: bottom block (pocket, peg, counterbored screw), top block
// (pocket, peg hole, insert hole; printed face up like the real top case) and a
// tab strip. Stack gasket / tab / gasket, close with an M3 x 10: it should squeeze, not bottom out.
module test_gasket() {
    L = 40;
    D = 24;
    hb = screw_length - insert_engagement + 2;     // bottom block: 2 mm counterbore + 6 mm of screw
    ht = w_rim - w_part;
    pw = tab_width + 2 * tab_clearance;
    pd = tab_length + tab_clearance - edge_clearance;
    difference() {
        cube([L, D, hb]);
        translate([L / 2 - pw / 2, -1, hb - (w_part - w_seat)]) cube([pw, pd + 1, 5]);
        translate([8, 12, -1]) cylinder(d = counterbore_diameter, h = 3);
        translate([8, 12, 2 + membrane]) cylinder(d = screw_clearance_diameter, h = hb);
    }
    translate([34, 16, hb - eps]) { cylinder(d = 3.0, h = 1.6 + eps); translate([0, 0, 1.6]) cylinder(d1 = 3.0, d2 = 2.2, h = 0.4); }
    translate([0, D + 6, 0]) difference() {
        cube([L, D, ht]);
        translate([L / 2 - pw / 2, D - pd, ht - (w_ceiling - w_part)]) cube([pw, pd + 1, 5]);
        translate([8, 12, ht - insert_hole_depth]) cylinder(d = insert_hole_diameter, h = insert_hole_depth + 1);
        translate([34, D - 16, ht - 2.5]) cylinder(d = 3.4, h = 3);
    }
    translate([0, -24, 0]) linear_extrude(plate_thickness) union() {
        square([L, 12]);
        translate([L / 2 - tab_width / 2, 11]) rrect(0, 0, tab_width, tab_length - edge_clearance + 1, 1.5);
    }
}

// ---------------------------------------------------------------------
//  Preview-only parts (assembled position)
// ---------------------------------------------------------------------
module view_plate(left) tilted() translate([0, 0, w_plate_bot]) linear_extrude(plate_thickness)
    if (left) plate_left_2d(); else plate_right_2d();

module view_pcb() tilted() translate([0, 0, w_pcb_bot]) linear_extrude(pcb_thickness)
    rrect(u_p, v_p, pcb_width, pcb_depth, 2);

module view_switches() tilted() for (k = keys) translate(concat(key_center(k), w_plate_top)) {
    linear_extrude(6.6, scale = 11 / 15.6) square(15.6, center = true);
    translate([0, 0, 6.6 - eps]) linear_extrude(3.6) {
        square([4.1, 1.3], center = true);
        square([1.3, 4.1], center = true);
    }
    translate([0, 0, -(w_plate_top - w_pcb_top)])
        linear_extrude(w_plate_top - w_pcb_top - plate_thickness) square(14, center = true);
}

keycap_set = "all"; // [all, alphas, mods, accents]
function is_accent(k) = (k[0] == 0 && k[1] == 0) || (k[1] == 2 && k[2] == 2.25);
function in_set(k) = keycap_set == "all" || (keycap_set == "accents" && is_accent(k))
    || (keycap_set == "mods" && !is_accent(k) && k[2] != 1)
    || (keycap_set == "alphas" && !is_accent(k) && k[2] == 1);

// Generic keycap: 18.1 mm footprint per 1u, skirt bottom 7 mm above the plate at rest
module view_keycaps(press = 0) tilted() for (k = keys) if (in_set(k))
    translate(concat(key_center(k), w_plate_top + 7.0 - press)) {
        kw = k[2] * U - 0.95;
        kd = U - 0.95;
        linear_extrude(8.5, scale = [(kw - 5.5) / kw, (kd - 5.5) / kd])
            offset(r = 1.2) offset(delta = -1.2) square([kw, kd], center = true);
    }

// Compressed gasket pads above and below every tab
module view_gaskets() tilted() for (t = tabs) {
    translate([0, 0, w_seat]) linear_extrude(g_c) gasket_2d(t);
    translate([0, 0, w_plate_top]) linear_extrude(g_c) gasket_2d(t);
}

module view_udb() tilted() {
    translate([udb_u0, udb_v1 - udb_d, udb_bottom]) cube([udb_w, udb_d, 1.6]);
    translate([udb_u0 + udb_w / 2 - 4.47, udb_v1 - 1.4 - 7.35, udb_bottom + 1.6]) cube([8.94, 7.35, 3.26]);
}

module assembly() {
    color("#3a3f47") { bottom_case(true); bottom_case(false); }
    color("#4a5059") { top_case(true); top_case(false); }
    color("#cfd4db") { view_plate(true); view_plate(false); }
    color("#1d6b47") view_pcb();
    color("#c9b458") view_gaskets();
    color("#e9e3d6") view_keycaps();
}

// ---------------------------------------------------------------------
if (part == "assembly") assembly();
else if (part == "bottom_left") bottom_case(true);
else if (part == "bottom_right") bottom_case(false);
else if (part == "top_left") top_print(true);
else if (part == "top_right") top_print(false);
else if (part == "plate_left") linear_extrude(plate_thickness) plate_left_2d();
else if (part == "plate_right") linear_extrude(plate_thickness) plate_right_2d();
else if (part == "plate_2d") plate_2d();
else if (part == "gaskets") gaskets_sheet();
else if (part == "gasket_2d") square([pad_w, pad_d]);
else if (part == "foam_left") foam_half(true);
else if (part == "foam_right") foam_half(false);
else if (part == "foam_2d") foam_2d();
else if (part == "test_switch") test_switch();
else if (part == "test_joint") test_joint();
else if (part == "test_gasket") test_gasket();
else if (part == "view_top_left") top_case(true);
else if (part == "view_top_right") top_case(false);
else if (part == "view_plate_left") view_plate(true);
else if (part == "view_plate_right") view_plate(false);
else if (part == "view_pcb") view_pcb();
else if (part == "view_switches") view_switches();
else if (part == "view_keycaps") view_keycaps();
else if (part == "view_keycaps_pressed") view_keycaps(4.0);
else if (part == "view_gaskets") view_gaskets();
else if (part == "view_udb") view_udb();
else if (part == "view_foam") tilted() { foam_half(true); foam_half(false); }

echo(str("MONAKA60 case ", case_w, " x ", case_d, " mm, front ", rim_z(0), " mm, back ", rim_z(case_d),
         " mm, layout ", layout, ", blockers ", blocker_style, ", connector ", connector));

// Machine-readable dimensions for verify.py
if (part == "info") echo(INFO = [th, Y0, Z0, w_pcb_bot, w_pcb_top, w_plate_bot, w_plate_top, w_rim, w_part,
    w_seat, w_ceiling, w_blocker, case_w, case_d, split_x, cav_w, cav_d, bezel_side, u_p, v_p,
    pcb_width, pcb_depth, [usb_slot_from, usb_slot_to], rim_z(0), rim_z(case_d),
    [for (s = screws) concat(screw_top(s), [screw_seat(s)])], gasket_thickness, g_c,
    [u_p + udb_port_x, udb_port_w, udb_v1], tab_clearance, len(keys), foam_thickness,
    [pad_w, pad_d], [for (t = tabs) tab_rect(t)]]);
