# Copyright 2026 HadalOS
# Distributed under the terms of the GNU General Public License v2

EAPI=8

inherit cargo git-r3 systemd

DESCRIPTION="The Hadal model host — Ollama-shaped inward, OpenAI-shaped outward"
HOMEPAGE="https://github.com/shmizzledizzle/HadalOS"

# See gui-wm/cusk for why this is a local path.
EGIT_REPO_URI="${HADALOS_GIT_REPO:-/home/shmizzy/Hadalpoint/Projects/HadalOS-Mobile}"
S="${WORKDIR}/${P}/src/hadald"

LICENSE="GPL-2"
SLOT="0"
KEYWORDS=""
PROPERTIES="live"

# reqwest is built with rustls rather than OpenSSL, so there is no system TLS
# library to depend on here. That is deliberate — see the comment on the
# dependency in Cargo.toml.
RDEPEND="
	acct-user/hadal
	acct-group/hadal
	sys-apps/systemd
"
DEPEND="${RDEPEND}"

src_unpack() {
	git-r3_src_unpack
	cargo_live_src_unpack
}

src_install() {
	cargo_src_install

	systemd_dounit "${WORKDIR}/${P}"/HadalOS/systemd/hadald.service

	# 0700 because upstream.key lives here. The daemon refuses a key file with
	# looser permissions, and a directory that is readable makes that check
	# most of the way pointless.
	diropts -m 0700 -o hadal -g hadal
	keepdir /etc/hadal
}

pkg_postinst() {
	if [[ -z ${REPLACING_VERSIONS} ]]; then
		elog "hadald will not start until it is told which model to use and"
		elog "given a key for the upstream endpoint."
		elog
		elog "  install -d -m 0700 -o hadal -g hadal /etc/hadal"
		elog "  echo 'HADAL_MODEL=<model-id>' > /etc/hadal/hadald.env"
		elog "  printf '%s' \"\$UPSTREAM_KEY\" > /etc/hadal/upstream.key"
		elog "  chown hadal:hadal /etc/hadal/upstream.key"
		elog "  chmod 600 /etc/hadal/upstream.key"
		elog
		elog "  systemctl enable --now hadald.service"
		elog
		ewarn "Backed by a remote endpoint, HadalOS is NOT local."
		ewarn
		ewarn "The safety property survives: proposals are still typed, still"
		ewarn "validated, still gated by polkit, because the broker was built"
		ewarn "not to trust the model wherever it runs. The privacy property"
		ewarn "does not survive. Portage build logs and journal excerpts carry"
		ewarn "hostnames, usernames, absolute paths and occasionally tokens."
		ewarn
		ewarn "What left the machine is recorded in /var/log/hadal/egress.log."
		ewarn "Bodies are NOT logged unless hadald is passed --log-bodies."
	fi
}
