# Copyright 2026 HadalOS
# Distributed under the terms of the GNU General Public License v2

EAPI=8

# A vendored dependency requires this. Without it the ebuild builds against
# whatever rustc the system happens to have and fails mid-compile on an older
# one, with an error naming a crate rather than a version requirement.
# Portage reports the needed value as a QA notice; the workspace declares the same.
RUST_MIN_VER="1.87"

inherit cargo git-r3 systemd

DESCRIPTION="HadalOS capability broker (org.hadal.Broker1) and its client"
HOMEPAGE="https://github.com/shmizzledizzle/HadalOS"

# See gui-wm/cusk for why this is a local path.
EGIT_REPO_URI="${HADALOS_GIT_REPO:-/home/shmizzy/Hadalpoint/Projects/HadalOS-Mobile}"

# The desktop cargo workspace. Note this is HadalOS/src, not src/ — the tracked
# src/hadal-brokerd at the repo root is hadal-brokerd-android, a different crate
# with no D-Bus surface and no executor. Building that one here would produce a
# package that merges cleanly and cannot broker anything.
S="${WORKDIR}/${P}/HadalOS/src"

LICENSE="GPL-2"
SLOT="0"
KEYWORDS=""
PROPERTIES="live"

# The broker and the CLI are one package because they are one workspace at one
# version and the CLI is useless without the broker. Splitting them would mean
# a version skew between a D-Bus interface and its only client.
RDEPEND="
	acct-user/hadal
	acct-group/hadal
	sys-apps/dbus
	sys-auth/polkit
	sys-apps/systemd
"
DEPEND="${RDEPEND}"

src_unpack() {
	git-r3_src_unpack
	cargo_live_src_unpack
}

src_install() {
	# A virtual manifest has no binaries of its own; each member is installed
	# by path or cargo refuses.
	cargo_src_install --path ./hadal-brokerd
	cargo_src_install --path ./hadal-cli

	# hadal-brokerd.service has ExecStart=/usr/libexec/hadal-brokerd. cargo
	# installs everything to /usr/bin, so the daemon is moved to match the
	# unit. If these two disagree the unit fails at start with a bare
	# "No such file or directory" and nothing points at the cause.
	exeinto /usr/libexec
	doexe "${ED}"/usr/bin/hadal-brokerd
	rm "${ED}"/usr/bin/hadal-brokerd || die

	# The `hadal` CLI stays in /usr/bin: it is what a user runs.

	# The bus policy. Without it dbus-daemon refuses the name claim and the
	# broker exits at startup having done nothing else wrong.
	insinto /usr/share/dbus-1/system.d
	doins "${S}"/../dbus/org.hadal.Broker1.conf

	# The activation file, which is a different thing from the policy above and
	# was missing until 2026-08-24. Policy says who may talk to the name;
	# activation says how the name comes to exist. With only the policy, the
	# first `hadal explain` reached the broker's bus name and got
	# ServiceUnknown — "not provided by any .service files" — which reads like
	# a missing package rather than a missing file inside this one.
	#
	# hadal-brokerd.service is Type=dbus with BusName=org.hadal.Broker1, so it
	# was always written to be started this way.
	insinto /usr/share/dbus-1/system-services
	doins "${S}"/../dbus/org.hadal.Broker1.service

	# The polkit actions. Without these every Mutate-tier capability is denied
	# with an authorization error that looks like a bug in the broker.
	insinto /usr/share/polkit-1/actions
	doins "${S}"/../policy/org.hadal.broker.policy

	systemd_dounit "${S}"/../systemd/hadal-brokerd.service
}

pkg_postinst() {
	if [[ -z ${REPLACING_VERSIONS} ]]; then
		elog "The broker is installed but not started. It requires hadald:"
		elog "  systemctl enable --now hadal-brokerd.service"
		elog
		elog "Then, as your own user and NOT as root:"
		elog "  hadal status"
		elog
		elog "Running the client as root is strictly worse than running it as"
		elog "yourself. polkit authorizes root for every capability without"
		elog "prompting, so 'sudo hadal' gets you the same capability set with"
		elog "the authorization gate switched off. The broker already holds the"
		elog "privilege and acts on your behalf."
	fi
}
