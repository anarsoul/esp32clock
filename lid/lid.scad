// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 Vasily Khoruzhick
//
// Raised back lid for the HU-058 clock case, for an adapter board that
// sits on pin headers and no longer clears the original lid.
//
// The lid is a shallow tray with the original plate's outline: its rim
// seats where the original plate's underside did, and its top stands
// `raise` above where the original's top was. The four screw posts and
// the two end walls are longer by the same amount. The screws go in
// from the outside, down a bore in each post, and their heads sit on a
// floor at the post's tip; the bore is deeper by the same amount, so
// the original screws still reach the case.
//
// Modelled in print orientation: the outer face is on the bed (z = 0)
// and everything grows upward, no supports.
//
//   openscad -o lid.stl lid.scad
//   openscad -o lid-template.stl -D 'part="template"' lid.scad
//
// The dimensions marked (caliper) were measured on the original lid.
// The template is a 0.6 mm test of the panel's outline and the post
// positions, for checking a change without printing the whole lid.
// The second print (raise 10, lid-v2-printed.stl) fits the case.

part = "lid"; // "lid" or "template"

// How much higher than the original the lid's top ends up.
raise = 10;

plate_t = 2.05;   // original plate thickness (caliper: 2.0 to 2.1)

// Screw posts.
post_outer_x = 148.10; // over the outer walls of the posts, along the clock (caliper)
post_d = 5.76;    // outer diameter (caliper)
post_dx = post_outer_x - post_d; // centre to centre along the clock
post_outer_y = 44.1; // over the outer walls of the posts, across (caliper)
post_dy = post_outer_y - post_d; // centre to centre across
orig_h = 12.20;   // original: outer face to post tip (caliper)
post_len = orig_h - plate_t; // original: plate underside to post tip
post_hole_d = 2.4; // screw clearance at the tip (the original is 2.2 by caliper; a printed hole closes up)
head_bore_d = 3.6; // the bore the screw head goes down, open on the outer face (caliper: 3.05; a screw stuck in a printed 3.2)
head_floor = 1.5;  // post tip to where the head seats
post_short = 0.2; // posts stop this short of the PCB, so the rim seats first

// Original plate.
post_far_to_end = 15.44; // a post's far wall to the plate's end (caliper)
end_from_post = post_far_to_end - post_d; // a post's outer wall to the plate's end
plate_l = post_outer_x + 2 * end_from_post; // along the clock
plate_w = 47.86; // across (caliper)
corner_r = 3;    // corner radius of the outline
fit = 0.2;       // taken off the outline all round, for the printer
// The overall length is over the lips. The panel itself stops short of
// that at each end, so the lips stand out past it (the first print had
// the panel at full length and sat on the case's ends).
body_back = 1.2; // the panel's end to the lip's outer face (caliper)
body_fit = 0.2;  // added to that at each end, for the printer

// Each short end has two walls with a channel between them for the
// case's end wall: a low lip along the plate's end and a taller rail
// just outside the posts.
lip_w = 37.67;   // across (caliper)
lip_t = 2.22;    // thickness (caliper); its outer face is the plate's end
lip_total = 6;   // original: outer face to the lip's edge (caliper)
lip_h = lip_total - plate_t; // below the plate's underside
rail_w = 38;     // across (caliper); its ends line up with the post centres
rail_chamfer = 3; // off each free corner, as on the original, for the case's corners
rail_t = 1.6;    // thickness (caliper)
rail_gap = 0.5;  // from the posts' outer walls to the rail (caliper)
rail_total = 8.5; // original: outer face to the rail's edge (caliper)
rail_h = rail_total - plate_t; // below the plate's underside
// Button end (+x): round cuts in the rail's free edge for the two buttons.
button_edge = 13.52; // a button hole's centre to the nearer long side (caliper)
button_hole_d = 3.84; // (caliper)
button_fit = 0.3;     // added to the diameter, for the printer
button_dy = plate_w - 2 * button_edge; // between the buttons
button_cut_r = (button_hole_d + button_fit) / 2;
// USB end (-x): a plain rail; the socket sits below it.

// Tray.
wall = 1.6;
top_t = 2;

// Vent slots: a group in the middle, where the original has its grille,
// and one each side so air moves past the thermistor.
vents = true;
vent_xs = [-41, 0, 41]; // group centres along the clock
vent_n = 5;
vent_l = 22;
vent_w = 1.6;
vent_pitch = 4;

$fn = 48;
eps = 0.01;

l = plate_l - 2 * fit;
w = plate_w - 2 * fit;
h = plate_t + raise; // bed to the seating plane

module outline(inset = 0) {
    r = max(corner_r - inset, 0.5);
    bl = l - 2 * (body_back + body_fit);
    offset(r) square([bl - 2 * inset - 2 * r, w - 2 * inset - 2 * r], center = true);
}

module post_places() {
    for (sx = [-1, 1], sy = [-1, 1])
        translate([sx * post_dx / 2, sy * post_dy / 2, 0]) children();
}

module tray() {
    chamfer = 0.5; // on the bed, against elephant's foot
    difference() {
        union() {
            hull() {
                linear_extrude(eps) outline(chamfer);
                translate([0, 0, chamfer]) linear_extrude(eps) outline();
            }
            translate([0, 0, chamfer]) linear_extrude(h - chamfer) outline();
        }
        translate([0, 0, top_t]) linear_extrude(h) outline(wall);
    }
}

module posts() {
    tip = h + post_len - post_short;
    post_places() {
        cylinder(d = post_d, h = tip);
        // a foot through the raised part, since the bore leaves a thin wall
        translate([0, 0, top_t - eps]) cylinder(d1 = post_d + 3, d2 = post_d, h = h - top_t);
    }
}

module post_holes() {
    tip = h + post_len - post_short;
    seat = tip - head_floor;
    cone = (head_bore_d - post_hole_d) / 2; // 45 degrees, prints without a bridge
    post_places() {
        translate([0, 0, -1]) cylinder(d = head_bore_d, h = seat - cone + 1);
        translate([0, 0, seat - cone - eps]) cylinder(d1 = head_bore_d, d2 = post_hole_d, h = cone + 2 * eps);
        cylinder(d = post_hole_d, h = tip + 1);
    }
}

// the rail's inner face, from the centre
function rail_x0() = post_dx / 2 + post_d / 2 + rail_gap;

module lips() {
    for (sx = [-1, 1])
        translate([sx * (l / 2 - lip_t / 2), 0, (h + lip_h) / 2])
            cube([lip_t, lip_w, h + lip_h], center = true);
}

// one rail, its inner face at x = 0, growing along +x
module rail() {
    top = h + rail_h;
    rotate([90, 0, 90]) linear_extrude(rail_t)
        polygon([[-rail_w / 2, 0], [rail_w / 2, 0], [rail_w / 2, top - rail_chamfer],
                 [rail_w / 2 - rail_chamfer, top], [-rail_w / 2 + rail_chamfer, top],
                 [-rail_w / 2, top - rail_chamfer]]);
}

module rails() {
    x0 = rail_x0();
    top = h + rail_h;
    // button end
    difference() {
        translate([x0, 0, 0]) rail();
        for (sy = [-1, 1])
            translate([x0 - 1, sy * button_dy / 2, top])
                rotate([0, 90, 0]) cylinder(r = button_cut_r, h = rail_t + 2);
    }
    // USB end
    translate([-x0, 0, 0]) mirror([1, 0, 0]) rail();
}

module vent_slots() {
    for (x = vent_xs, i = [0 : vent_n - 1])
        translate([x + (i - (vent_n - 1) / 2) * vent_pitch, 0, -1])
            linear_extrude(top_t + 2)
                hull() for (sy = [-1, 1])
                    translate([0, sy * (vent_l - vent_w) / 2]) circle(d = vent_w);
}

module lid() {
    difference() {
        union() {
            tray();
            posts();
            lips();
            rails();
        }
        post_holes();
        if (vents) vent_slots();
    }
}

module template() {
    difference() {
        linear_extrude(0.6) outline();
        post_places() translate([0, 0, -1]) cylinder(d = head_bore_d, h = 3);
        // a window to lift it out by
        translate([0, 0, -1]) linear_extrude(3) offset(2) square([60, 16], center = true);
    }
}

if (part == "template") template();
else lid();
