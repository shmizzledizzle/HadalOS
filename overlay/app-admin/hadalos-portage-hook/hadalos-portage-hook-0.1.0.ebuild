# Copyright 2026 HadalOS
# Distributed under the terms of the GNU General Public License v2

EAPI=8

DESCRIPTION="Records Portage build failures for analysis by Hadal"
HOMEPAGE="https://github.com/shmizzledizzle/HadalOS"
S="${WORKDIR}"

LICENSE="GPL-2"
SLOT="0"
KEYWORDS="~amd64"

RDEPEND="
	sys-apps/portage
	app-shells/bash
"
# hadal-brokerd is what makes the recorded failures useful, but it is not
# required: the hook records regardless, and `hadal explain` is what needs the
# broker. Keeping them separable means a build host can capture failures
# without running an assistant.

src_install() {
	insinto /etc/portage
	doins "${FILESDIR}"/portage-bashrc
	# Portage looks for this exact name; the shipped file only dispatches to
	# bashrc.d so admins can edit their own snippets without fighting updates.
	mv "${ED}"/etc/portage/portage-bashrc "${ED}"/etc/portage/bashrc || die

	insinto /etc/portage/bashrc.d
	doins "${FILESDIR}"/10-hadalos.bashrc

	keepdir /var/lib/hadalos/build-failures
	keepdir /var/log/portage/hadalos
}

pkg_postinst() {
	if [[ -z ${REPLACING_VERSIONS} ]]; then
		elog "Portage build failures are now recorded to"
		elog "  /var/lib/hadalos/build-failures/"
		elog "with the tail of each build log kept in"
		elog "  /var/log/portage/hadalos/"
		elog
		elog "Analyse the most recent one with:"
		elog "  hadal explain"
		elog
		elog "Run that as your own user, NOT as root. The broker holds the"
		elog "privilege and acts on your behalf; running the client as root"
		elog "only discards the polkit authorization check."
		elog
		elog "If you already had an /etc/portage/bashrc, Portage has saved the"
		elog "new one alongside it -- run etc-update or dispatch-conf and keep"
		elog "the bashrc.d dispatch loop, or add this line to your own file:"
		elog "  source /etc/portage/bashrc.d/10-hadalos.bashrc"
	fi

	if [[ -z ${PORTAGE_LOGDIR} ]]; then
		ewarn "PORTAGE_LOGDIR is not set in make.conf."
		ewarn "Build logs will be copied from the temporary build directory"
		ewarn "before it is cleaned, which works, but setting"
		ewarn "  PORTAGE_LOGDIR=\"/var/log/portage\""
		ewarn "gives better logs and keeps them across the whole merge."
	fi
}
