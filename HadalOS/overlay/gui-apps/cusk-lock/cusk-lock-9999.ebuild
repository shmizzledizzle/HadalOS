# Copyright 2026 HadalOS
# Distributed under the terms of the GNU General Public License v2

EAPI=8

# A vendored dependency requires this. Without it the ebuild builds against
# whatever rustc the system happens to have and fails mid-compile on an older
# one, with an error naming a crate rather than a version requirement.
# Portage reports the needed value as a QA notice; derived from Cargo.lock.
RUST_MIN_VER="1.89"

inherit cargo git-r3

DESCRIPTION="The HadalOS session locker — ext-session-lock-v1 with PAM"
HOMEPAGE="https://github.com/shmizzledizzle/HadalOS"

# See gui-wm/cusk for this, and for how to point it at a local checkout.
EGIT_REPO_URI="${HADALOS_GIT_REPO:-https://github.com/shmizzledizzle/HadalOS.git}"
S="${WORKDIR}/${P}/src/cusk-lock"

LICENSE="GPL-2"
SLOT="0"
KEYWORDS=""
PROPERTIES="live"

# sys-libs/pam is a link-time dependency here, unlike the dlopened libraries the
# other cusk clients carry — this one calls pam_start directly.
#
# The runtime half that is easy to miss: pam_unix verifies a password by
# executing /sbin/unix_chkpwd, which is setgid `shadow`. Without that helper an
# unprivileged locker cannot read /etc/shadow and every correct password is
# rejected — a working binary and an unopenable screen. It ships with
# sys-libs/pam, so depending on pam covers it, but the mechanism is worth
# writing down because the failure looks like a bug in this program.
RDEPEND="
	sys-libs/pam
	dev-libs/wayland
	x11-libs/libxkbcommon
"
DEPEND="${RDEPEND}"

src_unpack() {
	git-r3_src_unpack
	cargo_live_src_unpack
}

src_install() {
	cargo_src_install

	# Its own PAM service, installed verbatim rather than generated with
	# pam.eclass's pamd_mimic_system: the file in the tree carries the reasoning
	# for why it is auth+account and not a full stack, and a generated file
	# would drop it. cusk-lock falls back to system-auth when this is absent, so
	# the package still works without it — this makes the policy editable
	# without touching system-auth.
	insinto /etc/pam.d
	doins "${WORKDIR}/${P}"/HadalOS/pam.d/cusk-lock
}

pkg_postinst() {
	if [[ -z ${REPLACING_VERSIONS} ]]; then
		elog "cusk-lock locks the session through ext-session-lock-v1, which"
		elog "means the *compositor* holds the lock: the screen is not a window"
		elog "over your desktop, and killing this program does not unlock it."
		elog
		elog "Test it before trusting it, and test it nested rather than on the"
		elog "session you are using:"
		elog
		elog "  cusk                      # in a terminal, opens a nested window"
		elog "  WAYLAND_DISPLAY=cusk-2 cusk-lock"
		elog
		elog "Type your password and press Enter. The bar fills as you type,"
		elog "turns amber while PAM is asked, and red if it says no."
		elog
		ewarn "Nothing invokes this yet. The dock's Lock item stays disabled"
		ewarn "until you have unlocked a nested session yourself — a locker is"
		ewarn "the one component where the first real use should not be the"
		ewarn "first test."
	fi
}
