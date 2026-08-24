# Copyright 2026 HadalOS
# Distributed under the terms of the GNU General Public License v2

EAPI=8

inherit acct-user

DESCRIPTION="User for the Hadal model host"
ACCT_USER_ID=-1
ACCT_USER_GROUPS=( hadal )

# Device access is granted by the unit that needs it, not baked into the
# account, so that an accelerator-less install does not carry an account with
# access it never uses.
#
# `hadal-model.service` — the inference runtime — carries
# `SupplementaryGroups=render` and a DeviceAllow for `/dev/dri/renderD128`
# alone. `hadald.service` deliberately has neither: it holds the API key and
# must never hold a device node. (Until 2026-08-24 this comment named
# hadald.service as the unit granting `video render`, which stopped being true
# when the runtime was split out — see docs/compute.md §5c.)
ACCT_USER_HOME=/var/lib/hadal
ACCT_USER_SHELL=/sbin/nologin

# Required in global scope by acct-user.eclass, and it is what generates
# RDEPEND from ACCT_USER_GROUPS — note the eclass uses `RDEPEND+=`, so a
# hand-written `RDEPEND="acct-group/hadal"` is both redundant and, if assigned
# afterwards, silently discards what the eclass added.
#
# Omitting this call does not merely lose the dependency. acct-user_pkg_pretend
# checks a flag that only this function sets, so the package dies in the
# *pretend* phase with "acct-user_add_deps must have been called in global
# scope!" — which is what happened on the first attempt to merge
# sys-apps/hadal-brokerd, since nothing had ever merged this package before.
acct-user_add_deps

KEYWORDS="~amd64"
