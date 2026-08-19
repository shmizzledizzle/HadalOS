# Copyright 2026 HadalOS
# Distributed under the terms of the GNU General Public License v2

EAPI=8

inherit cargo git-r3

DESCRIPTION="The HadalOS application launcher"
HOMEPAGE="https://github.com/shmizzledizzle/HadalOS"

# See gui-wm/cusk for why this is a local path.
EGIT_REPO_URI="${HADALOS_GIT_REPO:-/home/shmizzy/Hadalpoint/Projects/HadalOS-Mobile}"
S="${WORKDIR}/${P}/src/cusk-launcher"

LICENSE="GPL-2"
SLOT="0"
KEYWORDS=""
PROPERTIES="live"

# See gui-apps/cusk-dock for why only one of these is visible to `ldd`.
RDEPEND="
	dev-libs/wayland
	media-libs/libglvnd
	media-libs/vulkan-loader
	x11-libs/libxkbcommon
"
DEPEND="${RDEPEND}"

# The launcher lists installed applications by reading desktop entries and
# resolves each icon through the standard lookup. Nothing to install for the
# HadalOS mark itself — it is include_bytes!'d into the binary at compile time,
# because an absolute path to artwork outside the repo would make the crate
# build on exactly one machine.
RDEPEND+="
	x11-themes/hicolor-icon-theme
"

src_unpack() {
	git-r3_src_unpack
	cargo_live_src_unpack
}

pkg_postinst() {
	if [[ -z ${REPLACING_VERSIONS} ]]; then
		elog "cusk runs the launcher on a binding — commands.launcher, default"
		elog "'cusk-launcher'. There is nothing to enable."
		elog
		elog "cusk classifies it as an overlay by app id on its first commit,"
		elog "so renaming the binary is not sufficient: OVERLAY_APP_ID in the"
		elog "compositor and the launcher's own app id have to agree, or it is"
		elog "managed as an ordinary window and gets tiled."
	fi
}
