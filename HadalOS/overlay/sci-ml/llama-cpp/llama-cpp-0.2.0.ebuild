# Copyright 2026 HadalOS
# Distributed under the terms of the GNU General Public License v2

EAPI=8

inherit cmake toolchain-funcs

DESCRIPTION="LLM inference in C/C++, with the Vulkan compute backend"
HOMEPAGE="https://github.com/ggml-org/llama.cpp"

# Why this exists when ::gentoo-zh already packages llama.cpp.
#
# It is not in ::gentoo — that tree carries the standalone sci-ml/ggml tensor
# library and sci-ml/ollama, and no llama.cpp. ::gentoo-zh has a good ebuild
# (it gets GGML_NATIVE=OFF right, and handles the prebuilt Web UI more neatly
# than this one, by adding it as a distfile rather than disabling it).
#
# But HadalOS is a distribution, and a distribution cannot tell its users to
# add a third-party overlay in order to install its own assistant. Depending on
# ::gentoo-zh would make the local model tier contingent on a tree HadalOS does
# not control, which is a worse dependency than one ebuild's worth of upkeep.
#
# Both ::gentoo-zh ebuilds carry empty KEYWORDS, so they are opt-in and this
# one — keyworded ~amd64 — is what ordinary resolution picks. Nothing is
# shadowed and nothing needs masking. If ::gentoo ever carries llama.cpp, this
# should be deleted rather than kept in step.
SRC_URI="https://github.com/ggml-org/llama.cpp/archive/refs/tags/v${PV}.tar.gz
	-> ${P}.tar.gz"
S="${WORKDIR}/llama.cpp-${PV}"

LICENSE="MIT"
SLOT="0"
KEYWORDS="~amd64"

# vulkan is on by default because it is the whole reason this package is in the
# overlay. docs/compute.md §3.1 picks Vulkan compute as HadalOS's execution
# layer, and §3.1a verified a real dispatch on the reference laptop's Iris Xe.
IUSE="+vulkan +server +tools openmp system-ggml test"

# tools/CMakeLists.txt only descends into server/ and cli/ when
# LLAMA_BUILD_SERVER is set, and the whole tools/ directory is gated on
# LLAMA_BUILD_COMMON AND LLAMA_BUILD_TOOLS. So server without tools silently
# builds nothing, which is the class of quiet failure this overlay exists to
# avoid.
REQUIRED_USE="server? ( tools )"

RESTRICT="!test? ( test )"

# system-ggml is pinned to the version llama.cpp actually vendors, and this is
# deliberately strict.
#
# llama.cpp v0.2.0 bundles ggml 0.21.0. ::gentoo currently carries 0.19.0 and
# 0.20.0. CMakeLists.txt line 206 is a bare `find_package(ggml REQUIRED)` with
# **no version constraint**, so pairing this with an older ggml does not fail
# cleanly — it fails as a compile or link error somewhere in the middle of a
# long build, on a fast-moving C API.
#
# Pinning to >=0.21.0 means the flag is either correct or unsatisfiable. It is
# unsatisfiable today, on purpose: better an unmet dependency than a build that
# takes twenty minutes to discover a header mismatch. When ::gentoo ships
# 0.21.0 this becomes the better choice, because the machine already carries
# libggml-vulkan.so and a second copy is pure duplication.
RDEPEND="
	system-ggml? ( >=sci-ml/ggml-0.21.0:=[vulkan?] )
	!system-ggml? (
		vulkan? ( media-libs/vulkan-loader )
	)
"
DEPEND="
	${RDEPEND}
	!system-ggml? (
		vulkan? ( dev-util/vulkan-headers )
	)
"
# The Vulkan backend compiles its compute shaders at build time —
# ggml/src/ggml-vulkan/CMakeLists.txt does find_package(Vulkan COMPONENTS glslc
# REQUIRED) — so glslc must exist on the build host. Same BDEPEND ::gentoo's
# sci-ml/ggml carries, for the same reason.
BDEPEND="
	!system-ggml? (
		vulkan? ( media-libs/shaderc )
	)
"

# Matching ::gentoo's sci-ml/ggml, which checks the toolchain rather than
# depending on an OpenMP package: libgomp ships with gcc, so there is nothing to
# pull in — but a compiler built without it fails in the middle of the build
# instead of at dependency resolution.
pkg_pretend() {
	[[ ${MERGE_TYPE} != binary ]] && use openmp && ! use system-ggml && tc-check-openmp
}

pkg_setup() {
	[[ ${MERGE_TYPE} != binary ]] && use openmp && ! use system-ggml && tc-check-openmp
}

src_configure() {
	local mycmakeargs=(
		-DLLAMA_BUILD_COMMON=ON
		-DLLAMA_BUILD_TOOLS=$(usex tools)
		-DLLAMA_BUILD_SERVER=$(usex server)
		-DLLAMA_BUILD_TESTS=$(usex test)
		-DLLAMA_BUILD_EXAMPLES=OFF

		# Not a preference — a requirement.
		#
		# LLAMA_BUILD_UI and LLAMA_USE_PREBUILT_UI both default to ON, and the
		# second one means what it says: tools/ui/CMakeLists.txt passes
		# HF_ENABLED and fetches a prebuilt bundle from a HuggingFace bucket
		# during the build. Portage's sandbox forbids network access in
		# src_compile, and an ebuild that reaches the internet mid-build is not
		# reproducible even where it is permitted.
		#
		# The Web UI is also not something HadalOS wants: hadald is the only
		# client of this server, it speaks the OpenAI-compatible HTTP API, and
		# the server is bound to loopback. A browser UI on that port is surface
		# area with no consumer.
		-DLLAMA_BUILD_UI=OFF
		-DLLAMA_USE_PREBUILT_UI=OFF

		-DLLAMA_BUILD_IS_DEV=OFF
		-DLLAMA_USE_SYSTEM_GGML=$(usex system-ggml)

		# Static, and this is not a preference either. Found by merging,
		# 2026-08-24.
		#
		# With shared libraries the vendored ggml installs its own
		# /usr/lib64/libggml*.so, headers and CMake package, which collide
		# file-for-file with sci-ml/ggml. Portage catches the collision and
		# refuses, which is the *good* outcome. The bad outcome is what
		# "resolving" it naively would produce:
		#
		#   ggml 0.19.0 (::gentoo) SONAME = libggml.so.0
		#   ggml 0.21.0 (vendored) SONAME = libggml.so.0
		#
		# Same SONAME, two minor versions apart, on a C API that moves fast.
		# Deleting our .so and keeping theirs leaves llama-server with a
		# DT_NEEDED on libggml.so.0 that resolves to the *older* ABI at
		# runtime — a crash or silent corruption instead of a merge-time
		# error. Installing ours over theirs breaks every other ggml consumer
		# the same way.
		#
		# So the two cannot coexist as shared libraries at all, and a blocker
		# would be wrong too: sci-ml/ggml is a legitimate package that other
		# things link. Static linkage sidesteps the question — ggml ends up
		# inside the llama binaries, nothing is exported, and the presence or
		# absence of sci-ml/ggml stops mattering.
		#
		# Costs paid knowingly: larger binaries, and no libllama for external
		# consumers. HadalOS has none — hadald speaks HTTP to llama-server and
		# links nothing (docs/compute.md §5a). Revisit if that changes, or when
		# ::gentoo ships ggml 0.21.0 and system-ggml becomes viable.
		-DBUILD_SHARED_LIBS=$(usex system-ggml)
	)

	if ! use system-ggml; then
		mycmakeargs+=(
			-DGGML_VULKAN=$(usex vulkan)
			-DGGML_OPENMP=$(usex openmp)
			-DGGML_BUILD_TESTS=OFF
			-DGGML_BUILD_EXAMPLES=OFF
			-DGGML_CCACHE=OFF

			# GGML_NATIVE defaults ON, which adds -march=native.
			#
			# ARCHITECTURE.md §0 lists a binhost as the release-engineering
			# plan, and -march=native produces packages that SIGILL on any
			# machine older than the builder. The build host is a 9800X3D
			# (Zen 5) and the reference laptop is Alder Lake — a binary tuned
			# for the former can carry instructions the latter does not have,
			# and the failure arrives at runtime rather than at merge time.
			-DGGML_NATIVE=OFF
		)
	fi

	cmake_src_configure
}

src_install() {
	cmake_src_install

	if ! use system-ggml; then
		# ggml/CMakeLists.txt:348 is a bare
		# `install(TARGETS ggml LIBRARY PUBLIC_HEADER)` with no option to
		# suppress it, and the CMake package files at :417 are unconditional
		# too. PUBLIC_HEADER installs regardless of static or shared, so
		# BUILD_SHARED_LIBS=OFF removes the libraries from the collision but
		# not the headers.
		#
		# These belong to sci-ml/ggml. We vendor ggml as an implementation
		# detail and must not publish its interface.
		rm -f "${ED}"/usr/include/ggml*.h "${ED}"/usr/include/gguf.h || die
		rm -rf "${ED}/usr/$(get_libdir)/cmake/ggml" || die
		rm -f "${ED}/usr/$(get_libdir)/pkgconfig/ggml.pc" || die

		# The static archives too. These do *not* collide — sci-ml/ggml ships
		# only shared libraries and no .a at all — but they are 100+ MB of an
		# implementation detail nothing links against. hadald reaches
		# llama-server over HTTP (docs/compute.md §5a); HadalOS has no consumer
		# of libggml in any form.
		rm -f "${ED}/usr/$(get_libdir)"/libggml*.a || die

		# The invariant is stronger than "does not collide", and deliberately:
		# this package vendors ggml as an implementation detail and exports
		# none of it, in any linkage. A future release growing a new exported
		# artifact fails here rather than in someone else's merge — which is
		# what happened on 2026-08-24, twice, and both times correctly.
		local stray
		stray=$(find "${ED}" \( -name 'libggml*' -o -name 'ggml-config*' \
			-o -name 'ggml*.h' -o -name 'gguf.h' \) 2>/dev/null)
		if [[ -n ${stray} ]]; then
			eerror "vendored ggml is still being exported:"
			eerror "${stray}"
			die "llama-cpp must not install any ggml artifact"
		fi
	fi
}

pkg_postinst() {
	if use vulkan; then
		elog "Vulkan backend built. Confirm the GPU is actually usable before"
		elog "trusting it with a model:"
		elog "    llama-bench -m <model.gguf> -ngl 99"
		elog ""
		elog "An installed Vulkan ICD is not evidence a shader ran — see"
		elog "src/sonar/probe/ in the HadalOS tree, and docs/compute.md §3.1a."
	fi
	if use server; then
		elog "hadald reaches this server over loopback and needs no API key for"
		elog "a local upstream (docs/compute.md §5a):"
		elog "    llama-server -m <model.gguf> --host 127.0.0.1 --port 8080 -ngl 99"
		elog "    hadald --serve --model <name> --upstream http://127.0.0.1:8080/v1"
		elog ""
		elog "Sizing the offload matters. docs/compute.md §3.2a measured that"
		elog "exceeding a cgroup MemoryMax= with GPU-resident pages does not"
		elog "swap — it returns VK_ERROR_DEVICE_LOST and kills the context."
	fi
}
