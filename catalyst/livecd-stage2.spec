# HadalOS live ISO, stage 2 — squashfs and rootfs preparation.
#
# ── Why there is no bootloader configuration here ────────────────────────
#
# Catalyst's livecd-stage2 can produce a bootable ISO itself, via livecd/cdtar
# plus its own isolinux/GRUB handling. HadalOS does not use that path.
#
# Limine is installed differently from either: it wants `limine bios-install`
# run against the finished image and its own limine.conf, neither of which
# catalyst's cdtar machinery knows how to drive. Bending catalyst into doing it
# would mean carrying a patched cdtar and hoping upstream never changes the
# step order.
#
# So catalyst does what it is genuinely good at — resolving, building and
# cleaning a rootfs, then squashing it — and scripts/mkiso.sh takes that output
# and assembles the bootable image with xorriso and Limine directly. The seam
# is clean and each half is independently testable, which the alternative is
# not.
#
# Consequently: no livecd/cdtar, and no boot/kernel section. The kernel is
# already in the rootfs as sys-kernel/hadalos-kernel, installed by our own
# kernel-install plugin, which is the same path a real installation takes.

subarch: amd64
version_stamp: hadalos-@TIMESTAMP@
target: livecd-stage2
rel_type: hadalos
profile: default/linux/amd64/23.0/systemd
snapshot_treeish: @TREEISH@
source_subpath: hadalos/livecd-stage1-amd64-hadalos-@TIMESTAMP@

portage_confdir: @REPO_DIR@/catalyst/portage_confdir
portage_prefix: hadalos

livecd/fstype: squashfs
livecd/volid: HadalOS-amd64-@TIMESTAMP@
livecd/iso: hadalos-amd64-@TIMESTAMP@.iso

# Toolchain and build machinery: several gigabytes that an installer image has
# no use for. The installed system gets them from the stage3.
livecd/unmerge:
	app-portage/gentoolkit
	dev-build/autoconf
	dev-build/automake
	dev-build/libtool
	dev-build/make
	sys-devel/binutils
	sys-devel/bison
	sys-devel/flex
	sys-devel/gcc
	sys-devel/gettext
	sys-devel/m4
	sys-devel/patch
	sys-kernel/linux-headers

livecd/empty:
	/root/.ccache
	/tmp
	/usr/include
	/usr/local
	/usr/share/doc
	/usr/share/gtk-doc
	/usr/share/info
	/usr/share/man
	/usr/src
	/var/cache
	/var/log
	/var/tmp

livecd/rm:
	/etc/*-
	/etc/*.old
	/etc/resolv.conf
	/root/.bash_history
	/usr/lib*/*.a
	/usr/lib*/*.la
