# Copyright 2026 HadalOS
# Distributed under the terms of the GNU General Public License v2

EAPI=8

inherit cargo git-r3

DESCRIPTION="The HadalOS dock — pinned applications and a system tray"
HOMEPAGE="https://github.com/shmizzledizzle/HadalOS"

# See gui-wm/cusk for why this is a local path.
EGIT_REPO_URI="${HADALOS_GIT_REPO:-/home/shmizzy/Hadalpoint/Projects/HadalOS-Mobile}"
S="${WORKDIR}/${P}/src/cusk-dock"

LICENSE="GPL-2"
SLOT="0"
KEYWORDS=""
PROPERTIES="live"

# `ldd` reports exactly one of these — libxkbcommon. Everything else iced needs
# is loaded at runtime through wgpu and the winit Wayland backend, so a missing
# one is a client that builds, installs, starts, and never appears:
#
#   media-libs/vulkan-loader  libvulkan.so.1        wgpu's primary backend
#   media-libs/libglvnd       libEGL.so.1           the GL fallback
#   dev-libs/wayland          libwayland-client.so.0, libwayland-egl.so.1
#
# iced also carries tiny-skia, so a machine with no working Vulkan ICD renders
# in software rather than failing outright. That is a good property and a bad
# diagnostic: the dock appears, slowly, and nothing says why.
RDEPEND="
	dev-libs/wayland
	media-libs/libglvnd
	media-libs/vulkan-loader
	x11-libs/libxkbcommon
"
DEPEND="${RDEPEND}"

# The tray reads each application's icon through the same lookup desktop entries
# use, so an icon theme is what makes it non-empty. Without one the dock runs
# and every tray slot is blank.
RDEPEND+="
	x11-themes/hicolor-icon-theme
"

src_unpack() {
	git-r3_src_unpack
	cargo_live_src_unpack
}

pkg_postinst() {
	if [[ -z ${REPLACING_VERSIONS} ]]; then
		elog "cusk starts the dock itself — commands.dock, default 'cusk-dock'."
		elog "There is nothing to enable."
		elog
		elog "The tray hosts org.kde.StatusNotifierWatcher on the session bus."
		elog "Exactly one process on a bus may own that name, so on a session"
		elog "where another shell already owns it the dock says so and carries"
		elog "on with an empty tray. That is correct: two watchers would mean"
		elog "applications registering with one and being displayed by the"
		elog "other."
		elog
		elog "An empty tray is also the normal state. Nothing appears until an"
		elog "application volunteers to register."
	fi
}
