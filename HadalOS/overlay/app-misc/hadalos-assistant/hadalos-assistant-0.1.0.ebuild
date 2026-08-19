# Copyright 2026 HadalOS
# Distributed under the terms of the GNU General Public License v2

EAPI=8

DESCRIPTION="HadalOS assistant — the capability broker and the model host"
HOMEPAGE="https://github.com/shmizzledizzle/HadalOS"
S="${WORKDIR}"

LICENSE="metapackage"
SLOT="0"
KEYWORDS=""

# Separable from the base on purpose. app-admin/hadalos-portage-hook records
# build failures whether or not anything is around to read them, so a build host
# can capture without hosting a model.
RDEPEND="
	sys-apps/hadal-brokerd
	sys-apps/hadald
"
