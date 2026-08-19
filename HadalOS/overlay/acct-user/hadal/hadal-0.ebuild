# Copyright 2026 HadalOS
# Distributed under the terms of the GNU General Public License v2

EAPI=8

inherit acct-user

DESCRIPTION="User for the Hadal model host"
ACCT_USER_ID=-1
ACCT_USER_GROUPS=( hadal )

# hadald.service sets SupplementaryGroups=video render for GPU inference. Those
# are granted by the unit rather than baked into the account, so that an
# accelerator-less install does not carry an account with device access it
# never uses.
ACCT_USER_HOME=/var/lib/hadal
ACCT_USER_SHELL=/sbin/nologin

KEYWORDS="~amd64"

RDEPEND="acct-group/hadal"
