# Copyright 2026 HadalOS
# Distributed under the terms of the GNU General Public License v2

EAPI=8

# A vendored dependency requires this. Without it the ebuild builds against
# whatever rustc the system happens to have and fails mid-compile on an older
# one, with an error naming a crate rather than a version requirement.
# Portage reports the needed value as a QA notice; derived from Cargo.lock.
RUST_MIN_VER="1.89"

inherit cargo git-r3

DESCRIPTION="The HadalOS keyboard shortcut list"
HOMEPAGE="https://github.com/shmizzledizzle/HadalOS"

# See gui-wm/cusk for this, and for how to point it at a local checkout.
EGIT_REPO_URI="${HADALOS_GIT_REPO:-https://github.com/shmizzledizzle/HadalOS.git}"
S="${WORKDIR}/${P}/src/cusk-keys"

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

src_unpack() {
	git-r3_src_unpack
	cargo_live_src_unpack
}

pkg_postinst() {
	if [[ -z ${REPLACING_VERSIONS} ]]; then
		elog "cusk runs this on a binding — commands.keys, default 'cusk-keys'."
		elog "Super + / (or Super + ?) shows the list. There is nothing to enable."
		elog
		elog "It renders cusk::bindings, the same table the compositor executes"
		elog "and prints at startup, so it cannot disagree with the session about"
		elog "which keys do what. That includes the modifier: CUSK_MOD is read"
		elog "from the environment cusk passes down, not from cusk.toml, so a"
		elog "nested session's list matches its actual bindings."
	fi
}
