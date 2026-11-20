#!/usr/bin/env bash
# SPDX-License-Identifier: MIT OR Apache-2.0
# Copyright (c) 2026 Vasily Khoruzhick
# Make the list of licences a release goes out with: the notices of
# every crate the firmware, the pack maker and its WebAssembly are
# built from, for every target a release builds (about.toml), after a
# preamble for what is not a crate (about.hbs). Needs cargo-about
# (`cargo install --locked --features cli cargo-about`). A dependency
# under a licence about.toml does not accept fails the run: read the
# licence, then add it there.
#
#   scripts/licenses.sh [out]      # default: target/licenses.txt
set -euo pipefail
cd "$(dirname "$0")/.."

out="${1:-target/licenses.txt}"
mkdir -p "$(dirname "$out")"
cargo about generate --workspace --locked --fail -o "$out" about.hbs
echo "$out: $(wc -c < "$out") bytes" >&2
