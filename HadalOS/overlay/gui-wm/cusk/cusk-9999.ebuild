# Copyright 2026 HadalOS
# Distributed under the terms of the GNU General Public License v2

EAPI=8

# A vendored dependency requires this. Without it the ebuild builds against
# whatever rustc the system happens to have and fails mid-compile on an older
# one, with an error naming a crate rather than a version requirement.
# Portage reports the needed value as a QA notice; this is the value it reported.
RUST_MIN_VER="1.88.0"

inherit cargo git-r3

DESCRIPTION="The HadalOS Wayland compositor — floating and dynamic tiling in one"
HOMEPAGE="https://github.com/shmizzledizzle/HadalOS"

# Upstream, which every other ebuild in this overlay points at.
#
# This defaulted to a path inside one developer's home directory until
# 2026-08-25, with a comment explaining that there was no published remote and
# that the working checkout on that machine was therefore the repository. That
# was true when it was written and stopped being true when the tree was pushed
# — the same "recorded once and then outlived" shape this tree keeps finding,
# except that here the stale value also published one developer's home
# directory to anyone who read the overlay.
#
# HADALOS_GIT_REPO still overrides it, and that is now the *development* path
# rather than the only path. Building your own commits means pointing it at a
# local checkout, because git-r3 fetches from this URI and will not see
# anything you have not pushed:
#
#     # /etc/portage/make.conf
#     HADALOS_GIT_REPO="/path/to/your/HadalOS-Mobile"
EGIT_REPO_URI="${HADALOS_GIT_REPO:-https://github.com/shmizzledizzle/HadalOS.git}"

# git-r3 checks out to ${WORKDIR}/${P}; the crate is one member of that tree.
S="${WORKDIR}/${P}/src/cusk"

LICENSE="GPL-2"
SLOT="0"
# Live ebuild: no keywords, and PROPERTIES=live so Portage does not try to
# apply the network sandbox to the `cargo fetch` in cargo_live_src_unpack.
KEYWORDS=""
PROPERTIES="live"

# Split deliberately. Everything in DEPEND is linked and would fail the build if
# absent; everything added in RDEPEND is *dlopened* and fails at runtime with a
# black screen instead. `ldd` on the built binary shows only the first set,
# which is why the second set is easy to omit and expensive to omit:
#
#   dev-libs/wayland    "could not load libwayland-server.so"  — no clients
#   media-libs/libglvnd "Failed to load LibEGL"                — nothing renders
#
# smithay is built with use_system_lib, so libwayland is opened through dlib
# rather than linked, and the GL renderer resolves libEGL the same way.
DEPEND="
	dev-libs/expat
	dev-libs/libevdev
	dev-libs/libinput
	media-libs/mesa
	sys-apps/systemd
	sys-auth/seatd
	sys-libs/mtdev
	x11-libs/libdrm
	x11-libs/libxkbcommon
"
RDEPEND="
	${DEPEND}
	dev-libs/wayland
	media-libs/libglvnd
"

# The panel and titlebars rasterise text with fontdue against a font found on
# the system — text.rs FONT_CANDIDATES, nothing bundled. With none of them
# installed the compositor runs and every label is empty, which reads as a
# rendering bug rather than a missing font. DejaVu is the candidate most likely
# to already be present.
RDEPEND+="
	media-fonts/dejavu
"

src_unpack() {
	git-r3_src_unpack
	cargo_live_src_unpack
}

src_install() {
	cargo_src_install

	# The session wrapper, not the bare binary, is what the display manager
	# runs. See the comments in it for why the arguments are not optional.
	dobin "${FILESDIR}"/cusk-session

	insinto /usr/share/wayland-sessions
	doins "${FILESDIR}"/cusk.desktop
}

pkg_postinst() {
	if [[ -z ${REPLACING_VERSIONS} ]]; then
		elog "Cusk is installed and offered as a session at the login screen."
		elog
		elog "It is NOT the default. Pick 'HadalOS (Cusk)' from the session"
		elog "menu; your existing desktop stays where it is and stays default."
		elog "That is deliberate — on Wayland a compositor crash takes every"
		elog "client with it, so cusk should earn the default rather than be"
		elog "given it."
		elog
		elog "Before trusting it with a session, confirm it can drive the"
		elog "display at all. This is safe from inside your current desktop:"
		elog "  cusk --probe-drm"
		elog
		elog "The config is a text file with a typed schema. It does not exist"
		elog "until you write one, and every setting is at its default until"
		elog "then. Edit it with:"
		elog "  cusk-settings"
		elog
		elog "If the session ends immediately, check the journal:"
		elog "  journalctl --user -t cusk -b"
	fi
}
