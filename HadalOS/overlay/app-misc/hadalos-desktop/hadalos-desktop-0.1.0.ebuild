# Copyright 2026 HadalOS
# Distributed under the terms of the GNU General Public License v2

EAPI=8

DESCRIPTION="HadalOS desktop — the Cusk compositor and its shell"
HOMEPAGE="https://github.com/shmizzledizzle/HadalOS"
S="${WORKDIR}"

LICENSE="metapackage"
SLOT="0"
# The members are live ebuilds, so this cannot be keyworded stable-ish without
# implying a stability its dependencies do not have.
KEYWORDS=""

# cusk spawns the dock itself (commands.dock, default "cusk-dock") and runs the
# launcher on a binding (commands.launcher, default "cusk-launcher"). Both
# defaults are bare names resolved beside cusk and then on PATH, so a desktop
# missing them starts fine and silently has no dock and a keybinding that
# logs "could not run cusk-launcher" where nobody is looking. They are
# dependencies of a working desktop, not optional extras.
RDEPEND="
	gui-wm/cusk
	gui-apps/cusk-dock
	gui-apps/cusk-launcher
	gui-apps/cusk-settings
"
