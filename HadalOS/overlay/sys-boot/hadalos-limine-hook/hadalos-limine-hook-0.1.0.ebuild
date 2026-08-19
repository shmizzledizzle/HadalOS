# Copyright 2026 HadalOS
# Distributed under the terms of the GNU General Public License v2

EAPI=8

# systemd_dounit lives in systemd.eclass. Without this inherit the call in
# src_install is merely a "command not found" QA notice, the package still
# merges, and hadalos-mark-boot-good.service is silently never installed —
# leaving last-known-good pinning looking present and recording nothing.
inherit systemd

DESCRIPTION="kernel-install integration and last-known-good pinning for Limine"
HOMEPAGE="https://github.com/shmizzledizzle/HadalOS"
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
	dobin "${FILESDIR}"/hadalos-limine-update
	dobin "${FILESDIR}"/hadalos-mark-boot-good

	exeinto /usr/lib/kernel/install.d
	doexe "${FILESDIR}"/90-hadalos-limine.install

	systemd_dounit "${FILESDIR}"/hadalos-mark-boot-good.service
	systemd_dounit "${FILESDIR}"/hadalos-mark-boot-good.timer

	keepdir /etc/hadalos/limine.d
}

pkg_postinst() {
	if [[ -z ${REPLACING_VERSIONS} ]]; then
		elog "HadalOS's Limine integration is installed but not yet active."
		elog
		elog "1. Select the layout, so kernel-install routes here:"
		elog "     echo 'layout=hadalos' >> /etc/kernel/install.conf"
		elog
		elog "2. Set the kernel command line — this file is the source of"
		elog "   truth for every generated entry:"
		elog "     echo 'root=UUID=... rw' > /etc/kernel/cmdline"
		elog
		elog "3. Install Limine itself to the ESP and enroll it, per"
		elog "   https://wiki.gentoo.org/wiki/Limine"
		elog
		elog "4. Reinstall your kernel(s) to populate /boot/hadalos, then:"
		elog "     hadalos-limine-update"
		elog
		elog "5. Enable last-known-good pinning — the TIMER, not the service."
		elog "   The service is started by the timer and must not be wanted"
		elog "   by any boot target, or it blocks the transaction it is"
		elog "   waiting to see complete:"
		elog "     systemctl enable --now hadalos-mark-boot-good.timer"
		elog
		elog "   If your ESP is not at /boot, override the boot root to match"
		elog "   what 'kernel-install inspect' reports:"
		elog "     systemctl edit hadalos-mark-boot-good.service"
		elog "       [Service]"
		elog "       Environment=KERNEL_INSTALL_BOOT_ROOT=/efi"
		elog "       ReadWritePaths="
		elog "       ReadWritePaths=/etc/hadalos /efi"
		elog "     systemctl edit hadalos-mark-boot-good.timer"
		elog "       [Unit]"
		elog "       ConditionPathExists="
		elog "       ConditionPathExists=/efi/hadalos"
		elog
		elog "limine.conf is generated. Local additions belong in"
		elog "/etc/hadalos/limine.d/*.conf, which are appended verbatim."
	fi
}
