# Copyright 2026 HadalOS
# Distributed under the terms of the GNU General Public License v2

EAPI=8

DESCRIPTION="HadalOS system identity — /etc/os-release"
HOMEPAGE="https://github.com/shmizzledizzle/HadalOS"
S="${WORKDIR}"

LICENSE="GPL-2"
SLOT="0"
KEYWORDS="~amd64"

# baselayout owns /usr/lib/os-release as a regular file and /etc/os-release as a
# symlink to it. This package does not fight either of them.
#
# os-release(5) is explicit that /etc/os-release takes precedence over the
# /usr/lib copy, and that the /etc entry may be a real file rather than the
# symlink — the split exists precisely so that a derived system can override the
# vendor identity without owning the vendor file. So this installs a real file
# at /etc/os-release and leaves baselayout's copy untouched.
#
# Getting there requires removing baselayout's symlink first — see pkg_preinst
# for what happens if you do not, which is worse than it looks.
#
# Every future baselayout upgrade will offer to put its symlink back over the
# real file. /etc is config-protected, so that arrives as a ._cfg file and a
# prompt rather than silently; declining is the correct answer.
RDEPEND="sys-apps/baselayout"

src_install() {
	insinto /etc
	doins "${FILESDIR}"/os-release
}

pkg_preinst() {
	# Remove baselayout's symlink before merging, or this package does the
	# opposite of what it intends.
	#
	# baselayout ships /etc/os-release as a symlink to ../usr/lib/os-release.
	# Merging a regular file over it does not replace it: /etc is
	# config-protected, Portage sees an existing file *through* the symlink,
	# and stages ._cfg0000_os-release. etc-update then merges that — and it
	# follows the symlink too, writing HadalOS's content into
	# /usr/lib/os-release, which belongs to baselayout.
	#
	# Observed on this machine 2026-08-19. It looks like it worked, because
	# `. /etc/os-release` reports HadalOS either way. What it actually leaves
	# behind is a baselayout-owned file whose checksum no longer matches its
	# CONTENTS, in /usr/lib — which is NOT in CONFIG_PROTECT. The next
	# baselayout upgrade overwrites it with Gentoo's version, without a
	# prompt, and the identity silently reverts.
	#
	# Unlinking first means there is nothing for CONFIG_PROTECT to protect, so
	# the regular file merges directly and os-release(5) precedence — /etc
	# wins over /usr/lib — does the rest, with each file owned by the package
	# that ships it.
	if [[ -L ${EROOT}/etc/os-release ]]; then
		elog "Removing baselayout's /etc/os-release symlink so this package's"
		elog "file can be a real file. /usr/lib/os-release is left to baselayout."
		rm -f "${EROOT}/etc/os-release" || die
	fi
}

pkg_postinst() {
	elog "Confirm the identity with:"
	elog "  . /etc/os-release && echo \"\$PRETTY_NAME (\$ID, like \$ID_LIKE)\""
	elog
	elog "and confirm it is a real file rather than a symlink, which is the"
	elog "difference between an identity that survives a baselayout upgrade"
	elog "and one that silently reverts on the next one:"
	elog "  test -L /etc/os-release && echo WRONG || echo 'real file, correct'"
	elog
	elog "ID becomes 'hadalos' and ID_LIKE stays 'gentoo'. Portage does not"
	elog "read os-release at all, so emerge is unaffected; anything that does"
	elog "read it and understands ID_LIKE keeps taking the Gentoo path."
	elog
	elog "Future sys-apps/baselayout upgrades will offer to restore their own"
	elog "os-release symlink over this file. Keeping this file is the answer."
}
