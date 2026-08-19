# Copyright 2026 HadalOS
# Distributed under the terms of the GNU General Public License v2

EAPI=8

inherit kernel-build

BASE_P=linux-$(ver_cut 1-2)

DESCRIPTION="Mainline Linux, built with the HadalOS configuration"
HOMEPAGE="
	https://github.com/shmizzledizzle/HadalOS
	https://www.kernel.org/
"
SRC_URI="
	https://cdn.kernel.org/pub/linux/kernel/v$(ver_cut 1).x/${BASE_P}.tar.xz
"
# x.y.0 releases have no incremental patch; every later point release does.
if [[ $(ver_cut 3) != 0 ]]; then
	SRC_URI+="
		https://cdn.kernel.org/pub/linux/kernel/v$(ver_cut 1).x/patch-${PV}.xz
	"
fi
S="${WORKDIR}/${BASE_P}"

LICENSE="GPL-2"
KEYWORDS="~amd64"

# Installing a kernel without the thing that writes the boot entry produces a
# machine that has a new kernel and cannot boot it.
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
PDEPEND="
	>=virtual/dist-kernel-${PV}
"

QA_FLAGS_IGNORED="
	usr/src/linux-.*/scripts/gcc-plugins/.*.so
	usr/src/linux-.*/vmlinux
"

src_prepare() {
	if [[ $(ver_cut 3) != 0 ]]; then
		eapply "${WORKDIR}/patch-${PV}"
	fi

	default

	# The baseline is upstream's own defconfig rather than a vendored
	# multi-thousand-line .config. That keeps files/hadalos.config a set of
	# reviewable decisions instead of an opaque blob, and means a kernel bump
	# inherits upstream's new defaults instead of silently keeping last
	# year's. KEYWORDS is ~amd64 only, so plain `defconfig` resolves to
	# x86_64_defconfig.
	emake defconfig

	kernel-build_merge_configs "${FILESDIR}/hadalos.config"
}

pkg_postinst() {
	kernel-build_pkg_postinst

	if [[ -z ${REPLACING_VERSIONS} ]]; then
		elog "The boot entry is written by sys-boot/hadalos-limine-hook via"
		elog "kernel-install. Confirm it landed before rebooting:"
		elog
		elog "    hadalos-limine-update && cat /boot/limine.conf"
		elog
		elog "The generated menu always keeps a last-known-good entry once one"
		elog "has been recorded. If this is your first HadalOS kernel there is"
		elog "no fallback yet -- keep your previous bootloader reachable."
	fi
}
