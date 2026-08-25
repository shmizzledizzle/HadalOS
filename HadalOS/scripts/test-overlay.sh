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
# NOT named `head`. A helper by that name shadows /usr/bin/head for the whole
# script, and a `grep ... | head -1` inside a check then silently calls this
# instead — consuming no stdin and printing "-1". The RUST_MIN_VER check did
# exactly that: it compared "-1" against "-1", took the pass branch for every
# file, and printed six confident `ok` lines while asserting nothing.
section() { printf '\n%s\n' "$1"; }

ebuilds=$(find "${overlay}" -name '*.ebuild' | sort)
[[ -n ${ebuilds} ]] || { echo "no ebuilds under ${overlay}"; exit 1; }

section "ebuilds parse as bash"
# An ebuild with a syntax error fails at metadata generation, which is loud —
# but it fails for every package in the repo at once, which is not obvious.
for f in ${ebuilds}; do
	if out=$(bash -n "${f}" 2>&1); then
		ok "${f#"${overlay}"/}"
	else
		bad "${f#"${overlay}"/}: ${out}"
	fi
done

section "every \${FILESDIR} reference exists"
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

section "every category is declared in profiles/categories"
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

section "declared categories are not stale"
for c in ${declared}; do
	if [[ -d ${overlay}/${c} ]]; then
		ok "${c} exists"
	else
		bad "${c} is declared but has no directory"
	fi
done

section "live ebuilds declare PROPERTIES=live"
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

section "acct-user ebuilds call acct-user_add_deps in global scope"
# acct-user.eclass generates RDEPEND from ACCT_USER_GROUPS inside this
# function, and acct-user_pkg_pretend checks a flag only it sets. Omitting the
# call does not lose a dependency quietly — it dies in the *pretend* phase,
# before anything is built, with a message about global scope.
#
# Nothing caught this because acct-user/hadal had never been merged: the first
# attempt was 2026-08-24, and it failed on exactly this. A static check is the
# right shape for it, since the failure is visible in the file.
for f in ${ebuilds}; do
	case ${f} in
		"${overlay}"/acct-user/*) ;;
		*) continue ;;
	esac
	if ! grep -q '^ACCT_USER_GROUPS=' "${f}"; then
		# No groups means the call is not required by the eclass.
		ok "${f#"${overlay}"/} (no ACCT_USER_GROUPS)"
	elif grep -qE '^acct-user_add_deps[[:space:]]*$' "${f}"; then
		ok "${f#"${overlay}"/}"
	else
		bad "${f#"${overlay}"/} sets ACCT_USER_GROUPS but never calls acct-user_add_deps — dies in pkg_pretend"
	fi
done

section "acct-user ebuilds do not hand-write RDEPEND"
# The eclass uses `RDEPEND+=`, so an assignment after the call silently
# discards what it added, and one before it is duplicated.
for f in ${ebuilds}; do
	case ${f} in
		"${overlay}"/acct-user/*) ;;
		*) continue ;;
	esac
	if grep -qE '^RDEPEND=' "${f}"; then
		bad "${f#"${overlay}"/} assigns RDEPEND; let acct-user_add_deps generate it"
	else
		ok "${f#"${overlay}"/}"
	fi
done

section "ebuilds do not fetch from somebody's home directory"
# Every live ebuild here defaulted to /home/<someone>/... until 2026-08-25,
# because the tree had no published remote and the working checkout was the
# repository. Publishing made that both wrong and a disclosure: an overlay that
# names a developer's home directory is unusable on any other machine *and*
# tells everyone who reads it what that directory is.
#
# HADALOS_GIT_REPO is still the override for local development. The point is
# that it is the override and not the default.
for f in ${ebuilds}; do
	if grep -qE '^[^#]*/home/' "${f}"; then
		bad "${f#"${overlay}"/} references a home directory — it will not fetch anywhere else"
	else
		ok "${f#"${overlay}"/}"
	fi
done

section "unit directives are in the section systemd reads them from"
# Three times now, in three different units. systemd does not fail on a
# directive in the wrong section — it logs "Unknown key ... ignoring" once at
# load and carries on, so the unit works, starts, and silently lacks the
# property the file says it has:
#
#   hadal-brokerd.service  JoinsNamespaceOf= in [Service]  — never shared
#                          hadald's namespace, while PrivateNetwork=yes made
#                          it look confined anyway
#   hadal-model.service    JoinsNamespaceOf= in [Service]  — same bug, copied
#   hadald.service         StartLimitBurst= / ConditionPathExists= in
#                          [Service] — written there while fixing the first two
#
# These are the [Unit] keys this tree has actually got wrong or would plausibly
# get wrong; it is not the complete list, which is `man systemd.unit`.
unit_only_keys="JoinsNamespaceOf StartLimitBurst StartLimitIntervalSec \
Requires Requisite Wants BindsTo PartOf Conflicts Before After OnFailure \
OnSuccess RequiresMountsFor StopWhenUnneeded RefuseManualStart RefuseManualStop"
for unit in systemd/*.service systemd/*.timer; do
	[[ -e ${unit} ]] || continue
	misplaced=""
	sect=""
	while IFS= read -r line; do
		case ${line} in
			"["*"]") sect=${line} ;;
			"#"*|"") ;;
			*=*)
				key=${line%%=*}
				# Continuation lines and indented values are not directives.
				[[ ${key} == "${key#[[:space:]]}" ]] || continue
				for k in ${unit_only_keys}; do
					if [[ ${key} == "${k}" && ${sect} != "[Unit]" ]]; then
						misplaced+=" ${key}${sect}"
					fi
				done
				;;
		esac
	done < "${unit}"
	if [[ -n ${misplaced} ]]; then
		bad "${unit} has [Unit] directives in the wrong section:${misplaced} — systemd ignores them"
	else
		ok "${unit}"
	fi
done

section "Type=dbus units have a D-Bus activation file"
# A unit with Type=dbus and BusName= is started *by the bus*, not by
# multi-user.target. Without a matching file in system-services/, the name is
# never provided and every call fails with ServiceUnknown — an error that names
# the bus, not the package that forgot the file.
#
# The bus policy in system.d/ is a different file and does not substitute: it
# says who may talk to a name, not how the name comes to exist. Both were
# present except the activation one, which is why this check exists.
for unit in systemd/*.service; do
	[[ -e ${unit} ]] || continue
	grep -q '^Type=dbus' "${unit}" || continue
	name=$(grep -E '^BusName=' "${unit}" | head -1 | cut -d= -f2)
	if [[ -z ${name} ]]; then
		bad "${unit} is Type=dbus but sets no BusName"
	elif [[ -e dbus/${name}.service ]]; then
		ok "${unit} -> dbus/${name}.service"
	else
		bad "${unit} is Type=dbus but dbus/${name}.service does not exist — calls will get ServiceUnknown"
	fi
done

section "D-Bus activation files are installed by an ebuild"
for f in dbus/*.service; do
	[[ -e ${f} ]] || continue
	base=$(basename "${f}")
	if grep -rqF "${base}" ${ebuilds}; then
		ok "${base}"
	else
		bad "${base} exists but no ebuild installs it"
	fi
done

section "live ebuilds are not keyworded"
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

section "cargo ebuilds inherit cargo and unpack the git tree"
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

section "every package is listed in portage/hadalos.accept_keywords"
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

section "cargo ebuilds set RUST_MIN_VER, before inherit"
# Portage emits this as a QA notice, which merges anyway. The cost lands on a
# machine with an older rustc than a vendored crate needs: the build fails
# mid-compile with an error naming the crate, not the toolchain requirement.
# RUST_MIN_VER is @PRE_INHERIT — set after `inherit`, it is read too late and
# silently does nothing, which looks exactly like setting it correctly.
for f in ${ebuilds}; do
	grep -q 'inherit.*cargo' "${f}" || continue
	name=${f#"${overlay}"/}
	minver_line=$(grep -n '^RUST_MIN_VER=' "${f}" | head -1 | cut -d: -f1)
	inherit_line=$(grep -n '^inherit ' "${f}" | head -1 | cut -d: -f1)
	if [[ -z ${minver_line} ]]; then
		bad "${name} has no RUST_MIN_VER"
	elif [[ ${minver_line} -gt ${inherit_line} ]]; then
		bad "${name} sets RUST_MIN_VER after inherit — it is PRE_INHERIT and will be ignored"
	else
		ok "${name} ($(grep '^RUST_MIN_VER=' "${f}" | cut -d'"' -f2))"
	fi
done

section "metapackage dependencies resolve inside this overlay or ::gentoo"
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

section "unit files referenced by ebuilds exist"
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

section "installed state matches what the ebuilds intend"
# Only meaningful on a machine that has merged these. Skipped elsewhere rather
# than failed, so the suite still runs on a build host.
if [[ -e /var/db/pkg/sys-apps/hadalos-release-0.1.0 ]]; then
	# The bug this exists for: merging a regular file over baselayout's
	# /etc/os-release symlink does not replace it. Portage stages a ._cfg file,
	# etc-update follows the symlink, and the content lands in
	# /usr/lib/os-release — a baselayout-owned file in a directory that is NOT
	# config-protected. `. /etc/os-release` reports HadalOS either way, so the
	# only visible difference is that the next baselayout upgrade silently
	# reverts the identity.
	# Three distinct end states, and only one is right. The missing case is the
	# one that passed a naive `test -L` check while `. /etc/os-release` failed
	# outright — Portage's CONTENTS claimed the file and the filesystem did not
	# have it.
	if [[ -L /etc/os-release ]]; then
		bad "/etc/os-release is a symlink — the identity will revert on the next baselayout upgrade"
	elif [[ ! -e /etc/os-release ]]; then
		bad "/etc/os-release does not exist, but hadalos-release records it — see the ebuild's re-merge hazard"
	elif ! grep -q '^ID=hadalos' /etc/os-release; then
		bad "/etc/os-release exists but does not identify as HadalOS"
	else
		ok "/etc/os-release is a real file identifying as HadalOS"
	fi

	if command -v md5sum >/dev/null && [[ -e /usr/lib/os-release ]]; then
		recorded=$(grep -h '^obj /usr/lib/os-release ' /var/db/pkg/sys-apps/baselayout-*/CONTENTS 2>/dev/null \
			| awk '{print $3}' | head -n1)
		actual=$(md5sum /usr/lib/os-release | cut -d' ' -f1)
		if [[ -z ${recorded} ]]; then
			ok "baselayout does not record /usr/lib/os-release; nothing to compare"
		elif [[ ${recorded} == "${actual}" ]]; then
			ok "/usr/lib/os-release still matches baselayout's checksum"
		else
			bad "/usr/lib/os-release was modified — it belongs to baselayout and is not config-protected"
		fi
	fi
else
	ok "hadalos-release not merged here; skipping installed-state checks"
fi

printf '\n%d passed, %d failed\n' "${pass}" "${fail}"
[[ ${fail} -eq 0 ]]
