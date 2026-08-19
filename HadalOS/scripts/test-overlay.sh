#!/usr/bin/env bash
# Static checks over the ::hadalos overlay. No root, no network, no Portage.
#
# These are the checks that would otherwise be discovered by an `emerge` that
# fails partway, or — worse, and this project has produced it twice — by an
# emerge that succeeds and installs something subtly wrong. Each check below
# corresponds to a failure that is silent or near-silent at merge time.
#
#   bash scripts/test-overlay.sh
set -uo pipefail

cd "$(dirname "${BASH_SOURCE[0]}")/.." || exit 1
overlay="overlay"

pass=0 fail=0
ok()   { printf '  ok      %s\n' "$1"; pass=$((pass + 1)); }
bad()  { printf '  FAIL    %s\n' "$1"; fail=$((fail + 1)); }
head() { printf '\n%s\n' "$1"; }

ebuilds=$(find "${overlay}" -name '*.ebuild' | sort)
[[ -n ${ebuilds} ]] || { echo "no ebuilds under ${overlay}"; exit 1; }

head "ebuilds parse as bash"
# An ebuild with a syntax error fails at metadata generation, which is loud —
# but it fails for every package in the repo at once, which is not obvious.
for f in ${ebuilds}; do
	if out=$(bash -n "${f}" 2>&1); then
		ok "${f#"${overlay}"/}"
	else
		bad "${f#"${overlay}"/}: ${out}"
	fi
done

head "every \${FILESDIR} reference exists"
# doins/dobin on a missing file dies, so this one is loud. It is cheap to check
# and it is the most common edit-time mistake: renaming a file in files/ and
# not the ebuild.
for f in ${ebuilds}; do
	d=$(dirname "${f}")
	refs=$(grep -oE '"?\$\{FILESDIR\}"?/[A-Za-z0-9._-]+' "${f}" 2>/dev/null \
		| sed -E 's|"?\$\{FILESDIR\}"?/||' | sort -u)
	for n in ${refs}; do
		if [[ -e ${d}/files/${n} ]]; then
			ok "${f#"${overlay}"/} -> ${n}"
		else
			bad "${f#"${overlay}"/} -> files/${n} does not exist"
		fi
	done
done

head "every category is declared in profiles/categories"
# An undeclared category is NOT an error. Portage simply does not see the
# packages in it — `emerge` reports "there are no ebuilds to satisfy", which
# reads like a typo in the atom rather than a missing line in a metadata file.
# app-admin was missing from this file while a package in it was installed.
declared=$(cat "${overlay}/profiles/categories")
for c in $(find "${overlay}" -mindepth 1 -maxdepth 1 -type d -printf '%f\n' \
		| grep -Ev '^(metadata|profiles)$' | sort); do
	if grep -qx "${c}" <<<"${declared}"; then
		ok "${c}"
	else
		bad "${c} has packages but is not in profiles/categories"
	fi
done

head "declared categories are not stale"
for c in ${declared}; do
	if [[ -d ${overlay}/${c} ]]; then
		ok "${c} exists"
	else
		bad "${c} is declared but has no directory"
	fi
done

head "live ebuilds declare PROPERTIES=live"
# Without it Portage applies the network sandbox to src_unpack, and
# cargo_live_src_unpack's fetch fails with a network error inside a phase that
# is not obviously doing networking.
for f in ${ebuilds}; do
	[[ ${f} == *-9999.ebuild ]] || continue
	if grep -q '^PROPERTIES=.*live' "${f}"; then
		ok "${f#"${overlay}"/}"
	else
		bad "${f#"${overlay}"/} is a 9999 ebuild without PROPERTIES=live"
	fi
done

head "live ebuilds are not keyworded"
# A keyworded live ebuild will be pulled in by a normal dependency resolution
# and rebuilt from a moving target without anyone asking for it.
for f in ${ebuilds}; do
	[[ ${f} == *-9999.ebuild ]] || continue
	if grep -qE '^KEYWORDS="[[:space:]]*"' "${f}"; then
		ok "${f#"${overlay}"/}"
	else
		bad "${f#"${overlay}"/} is a live ebuild with non-empty KEYWORDS"
	fi
done

head "cargo ebuilds inherit cargo and unpack the git tree"
# cargo_live_src_unpack dies unless git-r3_src_unpack has produced ${S} first.
# Getting this wrong fails at merge, loudly — but only for whoever merges it.
for f in ${ebuilds}; do
	grep -q 'inherit.*cargo' "${f}" || continue
	name=${f#"${overlay}"/}
	if grep -q 'git-r3_src_unpack' "${f}" && grep -q 'cargo_live_src_unpack' "${f}"; then
		ok "${name}"
	else
		bad "${name} inherits cargo but does not unpack git then vendor crates"
	fi
done

head "every package is listed in portage/hadalos.accept_keywords"
# A package in this overlay is unusable until it is accepted, and the failure
# is "All ebuilds that could satisfy X have been masked" — which reads as a
# Portage policy decision rather than as a file in this repo missing a line.
# Live ebuilds specifically need `**`; `~amd64` matches nothing on a package
# with no keywords, so getting this wrong looks identical to not listing it.
accept="portage/hadalos.accept_keywords"
for f in ${ebuilds}; do
	atom=$(dirname "${f#"${overlay}"/}")
	line=$(grep -E "^${atom//\//\\/}[[:space:]]" "${accept}" 2>/dev/null)
	if [[ -z ${line} ]]; then
		bad "${atom} is not listed in ${accept}"
	elif [[ ${f} == *-9999.ebuild ]] && [[ ${line} != *'**'* ]]; then
		bad "${atom} is a live ebuild but is not accepted with ** (got: ${line#"${atom}" })"
	else
		ok "${atom}"
	fi
done

head "metapackage dependencies resolve inside this overlay or ::gentoo"
# A metapackage naming a package that does not exist is the failure this whole
# layer is for: `emerge app-misc/hadalos` stops on the first missing atom and
# says nothing about the rest.
gentoo_repo=$(portageq get_repo_path / gentoo 2>/dev/null || echo /var/db/repos/gentoo)
for f in $(find "${overlay}/app-misc" -name '*.ebuild' 2>/dev/null | sort); do
	atoms=$(sed -n '/^RDEPEND="/,/^"/p' "${f}" | grep -oE '^\s+[a-z0-9-]+/[A-Za-z0-9_+-]+' | tr -d '\t ')
	for a in ${atoms}; do
		if [[ -d ${overlay}/${a} || -d ${gentoo_repo}/${a} ]]; then
			ok "${f#"${overlay}"/} -> ${a}"
		else
			bad "${f#"${overlay}"/} -> ${a} exists in neither ::hadalos nor ::gentoo"
		fi
	done
done

head "unit files referenced by ebuilds exist"
# systemd_dounit on a missing path dies. The subtler version of this bug already
# happened here once: systemd_dounit without `inherit systemd` is a QA notice,
# the package merges, and the unit is silently never installed.
for f in ${ebuilds}; do
	grep -q 'systemd_dounit' "${f}" || continue
	name=${f#"${overlay}"/}
	if grep -qE '^inherit .*\bsystemd\b' "${f}"; then
		ok "${name} inherits systemd"
	else
		bad "${name} calls systemd_dounit without inheriting systemd — the unit will NOT be installed"
	fi
done

printf '\n%d passed, %d failed\n' "${pass}" "${fail}"
[[ ${fail} -eq 0 ]]
