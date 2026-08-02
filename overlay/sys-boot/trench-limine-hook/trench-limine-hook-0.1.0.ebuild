# Copyright 2026 Trench Linux
# Distributed under the terms of the GNU General Public License v2

EAPI=8

DESCRIPTION="kernel-install integration and last-known-good pinning for Limine"
HOMEPAGE="https://github.com/shmizzledizzle/Trench"
S="${WORKDIR}"

LICENSE="GPL-2"
SLOT="0"
KEYWORDS="~amd64"

RDEPEND="
	sys-boot/limine
	sys-kernel/installkernel[systemd]
	app-shells/bash
	sys-apps/systemd
"

src_install() {
	dobin "${FILESDIR}"/trench-limine-update
	dobin "${FILESDIR}"/trench-mark-boot-good

	exeinto /usr/lib/kernel/install.d
	doexe "${FILESDIR}"/90-trench-limine.install

	systemd_dounit "${FILESDIR}"/trench-mark-boot-good.service

	keepdir /etc/trench/limine.d
}

pkg_postinst() {
	if [[ -z ${REPLACING_VERSIONS} ]]; then
		elog "Trench's Limine integration is installed but not yet active."
		elog
		elog "1. Select the layout, so kernel-install routes here:"
		elog "     echo 'layout=trench' >> /etc/kernel/install.conf"
		elog
		elog "2. Set the kernel command line — this file is the source of"
		elog "   truth for every generated entry:"
		elog "     echo 'root=UUID=... rw' > /etc/kernel/cmdline"
		elog
		elog "3. Install Limine itself to the ESP and enroll it, per"
		elog "   https://wiki.gentoo.org/wiki/Limine"
		elog
		elog "4. Reinstall your kernel(s) to populate /boot/trench, then:"
		elog "     trench-limine-update"
		elog
		elog "5. Enable last-known-good pinning:"
		elog "     systemctl enable trench-mark-boot-good.service"
		elog
		elog "limine.conf is generated. Local additions belong in"
		elog "/etc/trench/limine.d/*.conf, which are appended verbatim."
	fi
}
