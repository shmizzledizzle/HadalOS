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
# The consequence, stated so it is not a surprise: /etc is under CONFIG_PROTECT,
# so the file lands as /etc/._cfg0000_os-release and does nothing at all until
# etc-update or dispatch-conf is run. pkg_postinst says so. Every future
# baselayout upgrade will offer to put its symlink back, and declining is the
# correct answer.
RDEPEND="sys-apps/baselayout"

src_install() {
	insinto /etc
	doins "${FILESDIR}"/os-release
}

pkg_postinst() {
	elog "This package does NOT take effect on merge."
	elog
	elog "/etc is config-protected, so the new identity was written to"
	elog "  /etc/._cfg0000_os-release"
	elog "and /etc/os-release is still baselayout's symlink until you run:"
	elog "  etc-update      (or dispatch-conf)"
	elog "and accept the change."
	elog
	elog "Confirm with:"
	elog "  . /etc/os-release && echo \"\$PRETTY_NAME (\$ID, like \$ID_LIKE)\""
	elog
	elog "ID becomes 'hadalos' and ID_LIKE stays 'gentoo'. Portage does not"
	elog "read os-release at all, so emerge is unaffected; anything that does"
	elog "read it and understands ID_LIKE keeps taking the Gentoo path."
	elog
	elog "Future sys-apps/baselayout upgrades will offer to restore their own"
	elog "os-release symlink over this file. Keeping this file is the answer."
}
