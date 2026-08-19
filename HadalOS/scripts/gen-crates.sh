#!/usr/bin/env bash
# Generate the CRATES= list for a cargo.eclass ebuild from a Cargo.lock.
#
# cargo.eclass turns CRATES into SRC_URI via cargo_crate_uris, so every entry
# must be a crates.io package. Anything with a git source cannot be expressed
# that way and has to be handled by hand — so this refuses rather than emitting
# a list that would fetch the wrong thing, or nothing.
#
#   scripts/gen-crates.sh ../src/cusk/Cargo.lock
#
# The output goes in the ebuild verbatim, between CRATES=" and ".
set -euo pipefail

lock=${1:-}
if [[ -z ${lock} || ! -f ${lock} ]]; then
	echo "usage: ${0##*/} path/to/Cargo.lock" >&2
	exit 2
fi

# A [[package]] stanza with no `source` is a path dependency — the workspace
# member itself. It is built from S, not fetched, and listing it would send
# Portage looking on crates.io for a package that was never published.
#
# A `source` that is not the crates.io registry is a git or alternate-registry
# dependency. cargo_crate_uris cannot build a URL for those.
git_deps=$(awk '/^source = "git/ {found=1} END {print found+0}' "${lock}")
if [[ ${git_deps} != 0 ]]; then
	echo "${lock}: has git-sourced dependencies; CRATES cannot express those." >&2
	echo "Vendor them, or carry them as a separate SRC_URI entry." >&2
	exit 1
fi

awk '
	/^\[\[package\]\]/     { name=""; version=""; source=""; next }
	/^name = /             { gsub(/^name = "|"$/, ""); name=$0; next }
	/^version = /          { gsub(/^version = "|"$/, ""); version=$0; next }
	/^source = /           { source=$0 }
	/^$/ {
		# Emitted at the blank line that ends each stanza, so `source` has
		# been seen if it exists. Path deps (no source) are skipped.
		if (name != "" && source != "") print name "@" version
		name=""; version=""; source=""
	}
	END {
		if (name != "" && source != "") print name "@" version
	}
' "${lock}" | sort -u
