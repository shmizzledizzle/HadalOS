# HadalOS stage1 — bootstrap a HadalOS system from a Gentoo seed stage3.
#
# Seeded from an upstream Gentoo systemd stage3, which is the normal way to
# start a derivative: there is no HadalOS to build HadalOS with until this has
# run once. Afterwards, later builds can seed from our own stage3 instead by
# pointing source_subpath at it.
#
# @TIMESTAMP@ and @TREEISH@ are substituted by catalyst/build.sh.

subarch: amd64
target: stage1
version_stamp: hadalos-@TIMESTAMP@
rel_type: hadalos
profile: default/linux/amd64/23.0/systemd
snapshot_treeish: @TREEISH@
source_subpath: hadalos/stage3-amd64-systemd-seed
compression_mode: pixz

# The seed is a stock Gentoo stage3, so it must be brought up to the snapshot
# before it is used to build anything, or stage1 inherits whatever was current
# when that seed was published.
update_seed: yes
update_seed_command: --update --deep --newuse @world

portage_confdir: @REPO_DIR@/catalyst/portage_confdir
portage_prefix: hadalos

repos: @REPO_DIR@/overlay
