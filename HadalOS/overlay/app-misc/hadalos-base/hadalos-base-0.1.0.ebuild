# Copyright 2026 HadalOS
# Distributed under the terms of the GNU General Public License v2

EAPI=8

DESCRIPTION="HadalOS base — identity, boot layer, and Portage failure capture"
HOMEPAGE="https://github.com/shmizzledizzle/HadalOS"
S="${WORKDIR}"

LICENSE="metapackage"
SLOT="0"
KEYWORDS="~amd64"

# What makes a machine HadalOS rather than Gentoo, with no desktop and no
# assistant. This is the set a build host wants: it captures Portage failures
# and boots with last-known-good pinning, and it runs no model.
RDEPEND="
	sys-apps/hadalos-release
	sys-boot/hadalos-limine-hook
	app-admin/hadalos-portage-hook
"
