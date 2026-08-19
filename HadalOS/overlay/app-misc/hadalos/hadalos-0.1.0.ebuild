# Copyright 2026 HadalOS
# Distributed under the terms of the GNU General Public License v2

EAPI=8

DESCRIPTION="HadalOS — the whole system"
HOMEPAGE="https://github.com/shmizzledizzle/HadalOS"
S="${WORKDIR}"

LICENSE="metapackage"
SLOT="0"
KEYWORDS=""

RDEPEND="
	app-misc/hadalos-base
	app-misc/hadalos-desktop
	app-misc/hadalos-assistant
"

pkg_postinst() {
	elog "Installing the packages is not the whole conversion. Three steps"
	elog "cannot be done by an ebuild, because each one needs a decision:"
	elog
	elog "  1. Accept the new identity. /etc is config-protected, so:"
	elog "       etc-update        # keep /etc/os-release from hadalos-release"
	elog
	elog "  2. Point kernel-install at the HadalOS layout, if it is not"
	elog "     already, and enable last-known-good pinning:"
	elog "       systemctl enable --now hadalos-mark-boot-good.timer"
	elog
	elog "  3. Give hadald a model and a key — see its postinst."
	elog
	elog "Cusk is offered at the login screen and is NOT made default. Choose"
	elog "it deliberately, and keep your existing desktop installed until it"
	elog "has survived work you cared about."
}
