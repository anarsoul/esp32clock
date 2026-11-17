# SPDX-License-Identifier: MIT OR Apache-2.0
# Copyright (c) 2026 Vasily Khoruzhick
#
# The lids as STL files, rendered from lid.scad:
#
#   make                          lid.stl and lid-speaker.stl
#   make template                 lid-template.stl, the 0.6 mm fit test
#   make SCADFLAGS='-D raise=6'   either, with a parameter of lid.scad changed
#   make clean
#
# The *-printed.stl files are the ones that were printed and are kept
# as they are; nothing here makes or removes them.

OPENSCAD ?= openscad
SCADFLAGS ?=

.PHONY: all template clean
all: lid.stl lid-speaker.stl
template: lid-template.stl

lid.stl: lid.scad
	$(OPENSCAD) $(SCADFLAGS) -o $@ $<

lid-speaker.stl: lid.scad
	$(OPENSCAD) $(SCADFLAGS) -D speaker=true -o $@ $<

lid-template.stl: lid.scad
	$(OPENSCAD) $(SCADFLAGS) -D 'part="template"' -o $@ $<

clean:
	rm -f lid.stl lid-speaker.stl lid-template.stl
