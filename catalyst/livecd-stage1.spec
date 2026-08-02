# HadalOS live ISO, stage 1 — build everything the ISO will contain.
#
# Package selection only; no ISO is produced here. Kept deliberately smaller
# than Gentoo's installcd: this is not a general-purpose rescue disc, it is a
# HadalOS installer that happens to be usable for recovery.
#
# Note what is present that a stock installcd has no reason to carry: the
# broker, the assistant's model host, and the Portage hook. The ISO can
# explain its own failures. That is the point of the distribution.

subarch: amd64
version_stamp: hadalos-@TIMESTAMP@
target: livecd-stage1
rel_type: hadalos
profile: default/linux/amd64/23.0/systemd
snapshot_treeish: @TREEISH@
source_subpath: hadalos/stage3-amd64-hadalos-@TIMESTAMP@
compression_mode: pixz

portage_confdir: @REPO_DIR@/catalyst/portage_confdir
portage_prefix: hadalos

repos: @REPO_DIR@/overlay

livecd/use:
	dbus
	policykit
	systemd
	unicode
	-X
	-wayland

livecd/packages:
	app-admin/sudo
	app-arch/cpio
	app-arch/tar
	app-arch/unzip
	app-arch/xz-utils
	app-editors/nano
	app-editors/vim
	app-misc/tmux
	app-portage/gentoolkit
	app-portage/portage-utils
	app-shells/bash-completion
	app-text/tree
	dev-vcs/git
	net-misc/curl
	net-misc/dhcpcd
	net-misc/openssh
	net-misc/rsync
	net-misc/wget
	sys-apps/dmidecode
	sys-apps/gptfdisk
	sys-apps/hdparm
	sys-apps/iproute2
	sys-apps/nvme-cli
	sys-apps/pciutils
	sys-apps/smartmontools
	sys-apps/usbutils
	sys-block/parted
	sys-boot/efibootmgr
	sys-boot/limine
	sys-fs/btrfs-progs
	sys-fs/cryptsetup
	sys-fs/dosfstools
	sys-fs/e2fsprogs
	sys-fs/f2fs-tools
	sys-fs/lvm2
	sys-fs/mdadm
	sys-fs/squashfs-tools
	sys-fs/xfsprogs
	sys-kernel/linux-firmware
	sys-process/htop
	sys-process/lsof

	# HadalOS's own layer. sys-boot/hadalos-limine-hook comes in as a
	# dependency of the kernel.
	sys-kernel/hadalos-kernel
	app-admin/hadalos-portage-hook
