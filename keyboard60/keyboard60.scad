// =====================================================================
//  KB60: a 3D-printable 60% keyboard case + switch plate
//  Made to fit a 256 x 256 mm bed (Bambu Lab X1C / X1E / P1S / P1P)
//
//  * GH60-compatible tray mount: 6 x M2 standoffs at the standard
//    GH60 / Poker hole positions (DZ60, BM60, XD60, GH60, ... PCBs)
//  * The case is split into two halves that slide together on three
//    vertical T-keys (no hardware; add CA glue if you want it permanent)
//  * 1.5 mm MX plate, split along key boundaries so every switch
//    cutout stays whole and the seam hides under the keycaps
//  * Every part prints flat, in the orientation shown, with no supports
//
//  Coordinates: X = left->right, Y = front (user side) -> back, Z = up.
//  The PCB/plate stack lives in a tilted frame (rotated about X by
//  typing_angle). In that frame u = X, v = along the plate toward the
//  back, w = perpendicular to the plate (w = 0 is the case floor).
//
//  Export one part at a time, e.g.
//    openscad -o case_left.stl -D 'part="case_left"' keyboard60.scad
//  or run build.py to export and check everything.
// =====================================================================

/* [What to render] */
// Printable parts come out in print orientation; view_* parts are assembled previews
part = "assembly"; // [assembly, case_left, case_right, plate_left, plate_right, plate_full, plate_2d, test_switch, test_joint, foam_left, foam_right, foam_2d, view_plate_left, view_plate_right, view_pcb, view_switches, view_keycaps, view_keycaps_pressed, view_foam, info]

/* [Layout] */
layout = "ansi"; // [ansi:ANSI 60% (6.25u space), tsangan:Tsangan (7u space)]

/* [Case shape] */
// Plate / typing angle in degrees
typing_angle = 6; // [0:0.5:10]
// Side wall thickness (mm)
bezel_side = 6;
// Front bezel width at the rim (mm)
bezel_front = 6;
// Back bezel width at the rim (mm)
bezel_back = 8;
// Floor thickness at its thinnest (front) point (mm)
floor_min = 5;
// How far the rim sits above the top of the plate (mm)
rim_above_plate = 3;
// Plan-view corner radius (mm)
corner_radius = 4;
top_chamfer = 1.0;
// 45 degree chamfer on the bottom edge; hides elephant's foot
bottom_chamfer = 0.8;
inner_chamfer = 0.6;

/* [PCB and mounting] */
pcb_width = 285;
pcb_depth = 94.6;
pcb_thickness = 1.6;
// Clearance between PCB / plate edge and the case wall, per side
pcb_clearance = 0.5;
// Floor to PCB underside (room for hot-swap sockets, USB-C, case foam)
standoff_height = 4;
standoff_diameter = 5.6;
standoff_flare_diameter = 8.4;   // overlaps the side wall at the edge holes (a tangent contact would be non-manifold)
standoff_flare_height = 1.5;
// 3.2 = M2 heat-set insert (M2 x 3, 3.5 mm OD); 1.8 = self-tap the M2 screw straight into plastic
standoff_hole_diameter = 3.2;
standoff_hole_depth = 5.5;

/* [USB opening] */
// universal = wide slot that fits most GH60-style PCBs; custom = one opening at usb_center
usb_style = "universal"; // [universal, custom]
// Universal slot extent, measured from the PCB's left edge (mm)
usb_slot_from = 14;
usb_slot_to = 58;
// Custom opening: port centre measured from the PCB's left edge (mm)
usb_center = 30;
usb_width = 14;
// The opening spans this far below / above the PCB underside (mm)
usb_below_pcb = 5.4;
usb_above_pcb = 4.6;

/* [Split and joint] */
// Seam position relative to the case centre (mm, + moves it right)
split_offset = 0;
// T-shaped keys on the left half drop into matching slots in the right half.
// Their shoulders are square to the seam, so the seam can open by no more than key_clearance.
key_neck_width = 8;
key_neck_length = 4;
key_head_width = 16;
key_head_length = 5;
// Clearance on every face of the key slots (mm)
key_clearance = 0.15;
// Material left above each slot (mm)
key_roof = 1.2;
// Decorative V-groove along the seam, hides small mismatches (0 = off)
seam_groove = 0.6;

/* [Feet] */
feet = true;
// Recesses for stick-on bumpers
foot_diameter = 12.5;
foot_depth = 0.6;
foot_inset = 16;

/* [Case foam (optional)] */
// Sits on the floor under the PCB. Cut it from foam with foam_2d, or print foam_left / foam_right in TPU.
// 2 mm leaves room for 1.85 mm hot-swap sockets under the PCB.
foam_thickness = 2;

/* [Plate] */
plate_thickness = 1.5;
// MX cutout, 14.0 = spec. Fine-tune fit with Bambu Studio "X-Y hole compensation"
switch_cutout = 14.0;
// pcb = roomier cutouts for PCB-mount (screw-in) stabilizers; cherry = exact Cherry spec for plate-mount stabilizers
stab_style = "pcb"; // [pcb, cherry]
// Gap between the two plate halves (mm)
plate_split_gap = 0.2;
// Holes above the PCB screws, for the screw head and driver
screw_access_diameter = 4.6;
// Edge notches sit next to stabilizer cutouts, so they are a little smaller
screw_notch_diameter = 4.2;

/* [Hidden] */
$fn = 48;
eps = 0.01;
U = 19.05;                                   // 1 key unit

th = typing_angle;
cav_w = pcb_width + 2 * pcb_clearance;       // cavity, in the tilted frame
cav_d = pcb_depth + 2 * pcb_clearance;
cav_r = 1.0;
case_w = cav_w + 2 * bezel_side;

// Stack heights in the tilted frame (w = 0 is the floor)
w_pcb_bot = standoff_height;
w_pcb_top = w_pcb_bot + pcb_thickness;
w_plate_top = w_pcb_top + 5.0;               // MX: plate top is 5.0 mm above the PCB top
w_plate_bot = w_plate_top - plate_thickness;
w_rim = w_plate_top + rim_above_plate;

Y0 = bezel_front + w_rim * sin(th);          // world Y/Z of the tilted frame's origin
Z0 = floor_min;
case_d = bezel_front + cav_d * cos(th) + bezel_back;
split_x = case_w / 2 + split_offset;

function floor_z(y) = Z0 + (y - Y0) * tan(th);                   // cavity floor height at world Y
function rim_z(y) = Z0 + (y - Y0) * tan(th) + w_rim / cos(th);   // rim (top surface) height at world Y

module tilted() translate([0, Y0, Z0]) rotate([th, 0, 0]) children();

module rrect(x, y, w, d, r) {
    translate([x, y])
        if (r > 0) offset(r = r) offset(delta = -r) square([w, d]);
        else square([w, d]);
}

// ---------------------------------------------------------------------
//  Layout
// ---------------------------------------------------------------------
rows_common = [
    [1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 2],
    [1.5, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1.5],
    [1.75, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 2.25],
    [2.25, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 2.75]
];
rows_ansi = concat(rows_common, [[1.25, 1.25, 1.25, 6.25, 1.25, 1.25, 1.25, 1.25]]);
rows_tsangan = concat(rows_common, [[1.5, 1, 1.5, 7, 1.5, 1, 1.5]]);
rows = layout == "tsangan" ? rows_tsangan : rows_ansi;

// Plate seam, per row, in key units from the left edge. Each value sits on a
// key boundary (or in the open area beside the spacebar) so no cutout is cut.
seam_units = layout == "tsangan" ? [8, 7.5, 7.75, 7.25, 9] : [8, 7.5, 7.75, 7.25, 7.5];

function cumsum(v, i) = i <= 0 ? 0 : cumsum(v, i - 1) + abs(v[i - 1]);
function row_keys(r, y) = [for (i = [0 : len(r) - 1]) if (r[i] > 0) [cumsum(r, i), y, r[i]]];
keys = [for (y = [0 : len(rows) - 1]) each row_keys(rows[y], y)];   // [x_u, y_u, width_u]

kx0 = (15 * U - pcb_width) / 2;              // the key grid is centred on the PCB
ky0 = (5 * U - pcb_depth) / 2;
function ku(kx) = bezel_side + pcb_clearance + kx - kx0;       // key-grid mm -> u
function kv(ky) = pcb_clearance + pcb_depth - (ky - ky0);      // key-grid mm (down) -> v
function key_center(k) = [ku((k[0] + k[2] / 2) * U), kv((k[1] + 0.5) * U)];

// GH60 / Poker mounting holes, relative to the PCB centre (+x right, +y toward the user)
gh60_holes = [[-139, 9.2], [-117.3, -19.4], [-14.3, 0], [48, 37.9], [117.55, -19.4], [139, 9.2]];
function hole_uv(h) = [bezel_side + pcb_clearance + pcb_width / 2 + h[0], pcb_clearance + pcb_depth / 2 - h[1]];
// Plate access style per hole: 0 round hole, 1 notch from left edge,
// 2 notch from right edge, 3 opening that joins the two switch cutouts beside it
access_style = [1, 0, 3, 0, 0, 2];

// Stabilizer spacing from the switch centre, by key width
function stab_offset(w) = w >= 8 ? 66.675 : w >= 7 ? 57.15 : w >= 6.25 ? 50 : w >= 6 ? 47.625
                        : w >= 3 ? 19.05 : w >= 2 ? 11.938 : 0;
stab_w     = stab_style == "cherry" ? 6.65 : 7.0;
stab_back  = stab_style == "cherry" ? 5.53 : 5.73;   // extent toward the back (+v)
stab_front = stab_style == "cherry" ? 6.77 : 6.97;   // extent toward the user (-v)

// ---------------------------------------------------------------------
//  Plate (2D, in the u/v frame, seen from above)
// ---------------------------------------------------------------------
module plate_outline() rrect(bezel_side + pcb_clearance, pcb_clearance, pcb_width, pcb_depth, 1);

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

module screw_access() {
    for (i = [0 : len(gh60_holes) - 1]) {
        st = access_style[i];
        r = (st == 1 || st == 2 ? screw_notch_diameter : screw_access_diameter) / 2;
        translate(hole_uv(gh60_holes[i])) {
            if (st == 0) circle(r = r);
            if (st == 1) hull() { circle(r = r); translate([-20, -r]) square([1, 2 * r]); }
            if (st == 2) hull() { circle(r = r); translate([19, -r]) square([1, 2 * r]); }
            if (st == 3) square([U - switch_cutout + 1.0, 2 * r], center = true);
        }
    }
}

module plate_2d() difference() {
    plate_outline();
    for (k = keys) key_cutouts(k);
    screw_access();
}

// Everything left of the stepped plate seam
module plate_left_region() {
    for (r = [0 : 4]) {
        v_top = r == 0 ? 500 : kv(r * U);
        v_bot = r == 4 ? -500 : kv((r + 1) * U);
        translate([-500, v_bot]) square([ku(seam_units[r] * U) + 500, v_top - v_bot]);
    }
}

module plate_left_2d() intersection() {
    plate_2d();
    offset(delta = -plate_split_gap / 2) plate_left_region();
}

module plate_right_2d() difference() {
    plate_2d();
    offset(delta = plate_split_gap / 2) plate_left_region();
}

// ---------------------------------------------------------------------
//  Case
// ---------------------------------------------------------------------
module slab_world(inset, z)
    translate([0, 0, z]) linear_extrude(eps)
        rrect(inset, inset, case_w - 2 * inset, case_d - 2 * inset, corner_radius - inset);

// A thin slab lying parallel to the rim, dropped by dw, whose top-view
// footprint is the case outline shrunk by `inset`
module slab_top(inset, dw) {
    w = w_rim - dw;
    tilted() translate([0, 0, w - eps]) linear_extrude(eps)
        translate([0, (w * sin(th) - Y0) / cos(th)]) scale([1, 1 / cos(th)])
            rrect(inset, inset, case_w - 2 * inset, case_d - 2 * inset, corner_radius - inset);
}

module outer_body() hull() {
    slab_world(bottom_chamfer, 0);
    slab_world(0, bottom_chamfer);
    slab_top(0, top_chamfer);
    slab_top(top_chamfer, 0);
}

// Pocket for PCB + plate. Its walls are square to the plate, so the
// clearance is the same at every height of the stack.
module cavity() tilted() {
    linear_extrude(w_rim + 30) rrect(bezel_side, 0, cav_w, cav_d, cav_r);
    if (inner_chamfer > 0) hull() {
        translate([0, 0, w_rim - inner_chamfer]) linear_extrude(eps)
            rrect(bezel_side, 0, cav_w, cav_d, cav_r);
        grow = inner_chamfer + 2;
        translate([0, 0, w_rim + 2]) linear_extrude(eps)
            rrect(bezel_side - grow, -grow, cav_w + 2 * grow, cav_d + 2 * grow, cav_r + grow);
    }
}

module usb_cut() {
    x_a = bezel_side + pcb_clearance + (usb_style == "universal" ? usb_slot_from : usb_center - usb_width / 2);
    x_b = bezel_side + pcb_clearance + (usb_style == "universal" ? usb_slot_to : usb_center + usb_width / 2);
    w_lo = w_pcb_bot - usb_below_pcb;
    w_hi = w_pcb_bot + usb_above_pcb;
    // Runs along the plate (v), the direction the cable plugs in. It starts 2 mm
    // inside the cavity so a plug on a bottom-mounted port clears the floor too.
    tilted() translate([0, cav_d + 40, 0]) rotate([90, 0, 0]) linear_extrude(42)
        rrect(x_a, w_lo, x_b - x_a, w_hi - w_lo, 1.5);
}

module standoffs() tilted() for (h = gh60_holes) translate([hole_uv(h)[0], hole_uv(h)[1], 0]) {
    translate([0, 0, -1]) cylinder(d = standoff_diameter, h = standoff_height + 1);
    translate([0, 0, -1]) cylinder(d = standoff_flare_diameter, h = 1 + eps);
    cylinder(d1 = standoff_flare_diameter, d2 = standoff_diameter, h = standoff_flare_height);
}

module standoff_holes() tilted() for (h = gh60_holes) translate([hole_uv(h)[0], hole_uv(h)[1], 0]) {
    translate([0, 0, standoff_height - standoff_hole_depth])
        cylinder(d = standoff_hole_diameter, h = standoff_hole_depth + 1);
    translate([0, 0, standoff_height - 0.5])     // lead-in chamfer for the insert
        cylinder(d1 = standoff_hole_diameter, d2 = standoff_hole_diameter + 1.0, h = 0.5 + eps);
}

module feet_recesses() if (feet)
    for (x = [foot_inset, case_w - foot_inset], y = [foot_inset, case_d - foot_inset])
        translate([x, y, -1]) cylinder(d = foot_diameter, h = 1 + foot_depth);

module case_solid() difference() {
    union() {
        difference() { outer_body(); cavity(); usb_cut(); }
        standoffs();
    }
    standoff_holes();
    feet_recesses();
}

// ---------------------------------------------------------------------
//  Seam joint: three T-shaped keys in the floor wedge that slide together
//  vertically. The keys stand on the bed (left half); the slots are open at
//  the bottom and bridged over at the top (right half), so neither half
//  needs supports. Lower the right half straight down onto the left half.
// ---------------------------------------------------------------------
key_y = [for (f = [0.2, 0.5, 0.8]) Y0 + cav_d * cos(th) * f];
function key_top(yc) = floor_z(yc - key_head_width / 2 - key_clearance) - key_roof - 0.2;

module key_shape(x0, yc) {
    translate([x0 - 1, yc - key_neck_width / 2]) square([key_neck_length + 1 + eps, key_neck_width]);
    translate([x0 + key_neck_length, yc - key_head_width / 2]) square([key_head_length, key_head_width]);
}

// c = 0: the key itself, outer corners eased so they clear the slot's printed inside corners
// c > 0: the slot, grown by c on every face
module key_2d(x0, yc, c = 0) {
    if (c == 0) offset(r = 0.4) offset(delta = -0.4) key_shape(x0, yc);
    else offset(delta = c) key_shape(x0, yc);
}

module keys() for (yc = key_y) linear_extrude(key_top(yc)) key_2d(split_x, yc);
module key_slots() for (yc = key_y)
    translate([0, 0, -1]) linear_extrude(key_top(yc) + 0.2 + 1) key_2d(split_x, yc, key_clearance);

module seam_grooves() if (seam_groove > 0) {
    s = seam_groove * sqrt(2);
    for (y = [0, case_d])                                    // front and back faces
        translate([split_x, y, -1]) linear_extrude(100) rotate(45) square(s, center = true);
    translate([split_x, -1, 0]) rotate([-90, 0, 0])          // bottom
        linear_extrude(case_d + 2) rotate(45) square(s, center = true);
    tilted() translate([split_x, -30, w_rim]) rotate([-90, 0, 0])   // rim
        linear_extrude(cav_d + 60) rotate(45) square(s, center = true);
}

module half_box(left) {
    if (left) translate([-50, -50, -50]) cube([split_x + 50, case_d + 100, 200]);
    else translate([split_x, -50, -50]) cube([case_w - split_x + 50, case_d + 100, 200]);
}

module case_left() difference() {
    union() {
        intersection() { case_solid(); half_box(true); }
        keys();
    }
    seam_grooves();
}

module case_right() difference() {
    intersection() { case_solid(); half_box(false); }
    key_slots();
    seam_grooves();
}

// ---------------------------------------------------------------------
//  Optional case foam: fills the gap between floor and PCB to kill hollow sound.
//  Clears the standoffs and the area under the USB connector.
// ---------------------------------------------------------------------
module foam_2d() difference() {
    offset(delta = -0.75) rrect(bezel_side, 0, cav_w, cav_d, cav_r);
    for (h = gh60_holes) translate(hole_uv(h)) circle(d = standoff_flare_diameter + 1.5);
    x_a = bezel_side + pcb_clearance + (usb_style == "universal" ? usb_slot_from : usb_center - usb_width / 2);
    x_b = bezel_side + pcb_clearance + (usb_style == "universal" ? usb_slot_to : usb_center + usb_width / 2);
    translate([x_a - 2, cav_d - 14]) square([x_b - x_a + 4, 20]);
}

module foam_half(left) linear_extrude(foam_thickness) intersection() {
    foam_2d();
    if (left) translate([-50, -50]) square([split_x - 0.1 + 50, 300]);
    else translate([split_x + 0.1, -50]) square([300, 300]);
}

// ---------------------------------------------------------------------
//  Fit-test coupons (print these first: about 10 g and 20 minutes)
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

// A 30 mm slice of the seam joint (one key at the middle key's height).
// Lower the right block onto the left one: it should slide down by hand, snug, no wobble.
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

// ---------------------------------------------------------------------
//  Preview-only parts (assembled position)
// ---------------------------------------------------------------------
module view_plate(left) tilted() translate([0, 0, w_plate_bot]) linear_extrude(plate_thickness)
    if (left) plate_left_2d(); else plate_right_2d();

module view_pcb() tilted() translate([0, 0, w_pcb_bot]) linear_extrude(pcb_thickness) difference() {
    rrect(bezel_side + pcb_clearance, pcb_clearance, pcb_width, pcb_depth, 0.5);
    for (h = gh60_holes) translate(hole_uv(h)) circle(d = 2.2);
}

module view_switches() tilted() for (k = keys) translate(concat(key_center(k), w_plate_top)) {
    linear_extrude(6.6, scale = 11 / 15.6) square(15.6, center = true);
    translate([0, 0, 6.6 - eps]) linear_extrude(3.6) {
        square([4.1, 1.3], center = true);
        square([1.3, 4.1], center = true);
    }
    translate([0, 0, -(w_plate_top - w_pcb_top)])
        linear_extrude(w_plate_top - w_pcb_top - plate_thickness) square(14, center = true);
}

// Preview keycap subsets (for two-tone renders)
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

module assembly() {
    color("#3a3f47") case_left();
    color("#434952") case_right();
    color("#cfd4db") { view_plate(true); view_plate(false); }
    color("#1d6b47") view_pcb();
    color("#e9e3d6") view_keycaps();
}

// ---------------------------------------------------------------------
if (part == "assembly") assembly();
else if (part == "case_left") case_left();
else if (part == "case_right") case_right();
else if (part == "plate_left") linear_extrude(plate_thickness) plate_left_2d();
else if (part == "plate_right") linear_extrude(plate_thickness) plate_right_2d();
else if (part == "plate_full") linear_extrude(plate_thickness) plate_2d();
else if (part == "plate_2d") plate_2d();
else if (part == "test_switch") test_switch();
else if (part == "test_joint") test_joint();
else if (part == "foam_left") foam_half(true);
else if (part == "foam_right") foam_half(false);
else if (part == "foam_2d") foam_2d();
else if (part == "view_foam") tilted() { foam_half(true); foam_half(false); }
else if (part == "view_plate_left") view_plate(true);
else if (part == "view_plate_right") view_plate(false);
else if (part == "view_pcb") view_pcb();
else if (part == "view_switches") view_switches();
else if (part == "view_keycaps") view_keycaps();
else if (part == "view_keycaps_pressed") view_keycaps(4.0);

echo(str("KB60 case ", case_w, " x ", case_d, " mm, front height ", rim_z(0),
         " mm, back height ", rim_z(case_d), " mm, seam at x = ", split_x));

// Machine-readable dimensions for verify.py
if (part == "info") echo(INFO = [th, Y0, Z0, w_pcb_bot, w_pcb_top, w_plate_bot, w_plate_top, w_rim,
    case_w, case_d, split_x, cav_w, cav_d, bezel_side, pcb_clearance, pcb_width, pcb_depth,
    [for (h = gh60_holes) hole_uv(h)],
    usb_style == "universal" ? [usb_slot_from, usb_slot_to] : [usb_center - usb_width / 2, usb_center + usb_width / 2],
    rim_z(0), rim_z(case_d), standoff_height, standoff_hole_depth, foam_thickness]);
