# Copyright 2026 HadalOS
# Distributed under the terms of the GNU General Public License v2

EAPI=8

# A vendored dependency requires this. Without it the ebuild builds against
# whatever rustc the system happens to have and fails mid-compile on an older
# one, with an error naming a crate rather than a version requirement.
# Portage reports the needed value as a QA notice; derived from Cargo.lock.
RUST_MIN_VER="1.86"

inherit cargo git-r3 systemd

DESCRIPTION="The Hadal model host — Ollama-shaped inward, OpenAI-shaped outward"
HOMEPAGE="https://github.com/shmizzledizzle/HadalOS"

# See gui-wm/cusk for this, and for how to point it at a local checkout.
EGIT_REPO_URI="${HADALOS_GIT_REPO:-https://github.com/shmizzledizzle/HadalOS.git}"
S="${WORKDIR}/${P}/src/hadald"

LICENSE="GPL-2"
SLOT="0"
KEYWORDS=""
PROPERTIES="live"

# reqwest is built with rustls rather than OpenSSL, so there is no system TLS
# library to depend on here. That is deliberate — see the comment on the
# dependency in Cargo.toml.
# local-model installs hadal-model.service and pulls the inference runtime, so
# hadald can be pointed at loopback instead of a third party. Off by default:
# the runtime is a long build and is useless without weights, which are not
# packaged and cannot be — see pkg_postinst.
IUSE="local-model"

RDEPEND="
	acct-user/hadal
	acct-group/hadal
	sys-apps/systemd
	local-model? ( sci-ml/llama-cpp[server,vulkan] )
"
DEPEND="${RDEPEND}"

src_unpack() {
	git-r3_src_unpack
	cargo_live_src_unpack
}

src_install() {
	cargo_src_install

	systemd_dounit "${WORKDIR}/${P}"/HadalOS/systemd/hadald.service

	# Installed only with the runtime it drives. A unit whose ExecStart names a
	# binary the system does not have is the "recorded as though it were the
	# state" pattern this tree keeps finding, expressed as a dangling unit.
	if use local-model; then
		systemd_dounit "${WORKDIR}/${P}"/HadalOS/systemd/hadal-model.service
	fi

	# 0700 because upstream.key lives here. The daemon refuses a key file with
	# looser permissions, and a directory that is readable makes that check
	# most of the way pointless.
	diropts -m 0700 -o hadal -g hadal
	keepdir /etc/hadal
}

pkg_postinst() {
	if use local-model; then
		elog "Local tier. hadald classifies its upstream from the URL, so a"
		elog "loopback address needs no API key at all (docs/compute.md §5a):"
		elog
		elog "  install -d -m 0700 -o hadal -g hadal /etc/hadal"
		elog "  install -d -m 0755 -o hadal -g hadal /var/lib/hadal/models"
		elog "  # put a GGUF in /var/lib/hadal/models, then:"
		elog "  echo 'HADAL_MODEL_FILE=<file.gguf>' > /etc/hadal/model.env"
		elog "  echo 'HADAL_MODEL=reflex' > /etc/hadal/hadald.env"
		elog "  echo \"HADAL_UPSTREAM=http://127.0.0.1:8080/v1\" >> /etc/hadal/hadald.env"
		elog
		elog "  systemctl enable --now hadald.service hadal-model.service"
		elog
		elog "hadald owns the network namespace and hadal-model.service joins"
		elog "it, so hadald starts first and answers 502 until the model is"
		elog "listening. That ordering is deliberate."
		elog
		ewarn "Read docs/compute.md §3.2a before tightening MemoryMax= on"
		ewarn "hadal-model.service. Exceeding a cgroup limit with GPU-resident"
		ewarn "pages does not swap — it returns VK_ERROR_DEVICE_LOST and takes"
		ewarn "the whole Vulkan device with it. Too tight does not mean slow."
		elog
	fi

	if [[ -z ${REPLACING_VERSIONS} ]] && ! use local-model; then
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
