# Copyright 2026 HadalOS
# Distributed under the terms of the GNU General Public License v2

EAPI=8

inherit cargo git-r3

DESCRIPTION="The HadalOS settings editor — a round-tripping editor over cusk.toml"
HOMEPAGE="https://github.com/shmizzledizzle/HadalOS"

# See gui-wm/cusk for why this is a local path.
EGIT_REPO_URI="${HADALOS_GIT_REPO:-/home/shmizzy/Hadalpoint/Projects/HadalOS-Mobile}"
S="${WORKDIR}/${P}/src/cusk-settings"

LICENSE="GPL-2"
SLOT="0"
KEYWORDS=""
PROPERTIES="live"

# See gui-apps/cusk-dock. libxkbcommon is not in this one's `ldd` output at all
# — the editor has no layer-shell surface — but the winit Wayland backend loads
# it, so it is required all the same.
RDEPEND="
	dev-libs/wayland
	media-libs/libglvnd
	media-libs/vulkan-loader
	x11-libs/libxkbcommon
"
DEPEND="${RDEPEND}"

src_unpack() {
	git-r3_src_unpack
	cargo_live_src_unpack
}

src_install() {
	cargo_src_install

	# A desktop entry so the launcher can find it. Everything else in the
	# HadalOS shell is started by the compositor; this is the one piece a user
	# opens by name.
	insinto /usr/share/applications
	doins "${FILESDIR}"/cusk-settings.desktop
}

pkg_postinst() {
	if [[ -z ${REPLACING_VERSIONS} ]]; then
		elog "The settings editor and a text editor edit the same file:"
		elog "  \${XDG_CONFIG_HOME:-~/.config}/cusk/cusk.toml"
		elog
		elog "There is no apply button and no IPC. The editor writes the file,"
		elog "cusk notices it changed, and reloads. Hand edits are equally"
		elog "valid and are not clobbered — the GUI parses to a syntax tree and"
		elog "edits nodes in place, so comments, blank lines and ordering all"
		elog "survive a round trip."
		elog
		elog "The file does not exist until something writes one, and every"
		elog "setting is at its default until then."
		elog
		elog "A file that fails to parse is reported, not silently replaced."
		elog "Duplicate keys and repeated [section] headers are the usual"
		elog "cause, and cusk logs the complaint rather than quietly running"
		elog "on defaults."
	fi
}
