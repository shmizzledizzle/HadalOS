# HadalOS stage3 — the base system tarball.
#
# Consumes stage1's output. Modern catalyst goes stage1 -> stage3 directly;
# there is no separate stage2 in this flow.
#
# This is the artefact everything else is built from: the live ISO, the
# installed system, and the binhost. It deliberately does NOT contain the
# desktop or the assistant — those are packages, installed on top, so that a
# HadalOS server install is a real thing and not a desktop with pieces
# removed.

subarch: amd64
target: stage3
version_stamp: hadalos-@TIMESTAMP@
rel_type: hadalos
profile: default/linux/amd64/23.0/systemd
snapshot_treeish: @TREEISH@
source_subpath: hadalos/stage1-amd64-hadalos-@TIMESTAMP@
compression_mode: pixz

portage_confdir: @REPO_DIR@/catalyst/portage_confdir
portage_prefix: hadalos

repos: @REPO_DIR@/overlay

# Built packages are kept so the fleet does not rebuild the world. The path is
# relative to catalyst's binhost root.
binrepo_path: amd64/binpackages/hadalos/x86-64
