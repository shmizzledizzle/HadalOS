# Copyright 2026 HadalOS
# Distributed under the terms of the GNU General Public License v2

EAPI=8

# Mainline tip, straight from Linus's tree. This is the ebuild that makes the
# "most recent kernel" claim literal; the versioned ebuild beside it exists so
# releases are reproducible, because a tree that moves under you is not
# something to build an ISO from.
#
# Riding this is only defensible on HadalOS's reference hardware, where every
# graphics driver is in-tree and there is no out-of-tree module to lag behind
# a rebase. It is still tip: sys-boot/hadalos-limine-hook keeps a
# last-known-good entry precisely for the mornings this does not boot.

inherit git-r3 kernel-build

DESCRIPTION="Mainline Linux from torvalds/linux, built with the HadalOS configuration"
HOMEPAGE="
	https://github.com/shmizzledizzle/HadalOS
	https://git.kernel.org/pub/scm/linux/kernel/git/torvalds/linux.git
"

EGIT_REPO_URI="https://github.com/torvalds/linux.git"
EGIT_BRANCH="master"
# A full clone of this repository is several gigabytes and the history is of
# no use to a build. Shallow costs a re-fetch when the tip moves, which is the
# cheaper end of the trade by a wide margin.
EGIT_CLONE_TYPE="shallow"

S="${WORKDIR}/${P}"

LICENSE="GPL-2"
# Live ebuild: never keyworded, always requires an explicit unmask.
KEYWORDS=""
PROPERTIES="live"

RDEPEND="
	sys-boot/hadalos-limine-hook
"
BDEPEND="
	app-arch/cpio
	dev-lang/perl
	dev-libs/openssl
	sys-apps/kmod[tools]
	sys-devel/bc
	virtual/libelf
	virtual/pkgconfig
"

QA_FLAGS_IGNORED="
	usr/src/linux-.*/scripts/gcc-plugins/.*.so
	usr/src/linux-.*/vmlinux
"

src_unpack() {
	git-r3_src_unpack
}

src_prepare() {
	default

	emake defconfig
	kernel-build_merge_configs "${FILESDIR}/hadalos.config"
}

pkg_postinst() {
	kernel-build_pkg_postinst

	ewarn "This is mainline tip. It has had no stable release testing at all."
	ewarn "Before rebooting, confirm the fallback entry exists:"
	ewarn
	ewarn "    grep 'last known good' /boot/limine.conf"
	ewarn
	ewarn "If that prints nothing, you have no tested kernel to fall back to."
}
