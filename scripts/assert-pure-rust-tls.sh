#!/usr/bin/env bash
#
# Fail the build if a C-backed TLS/crypto crate enters the dependency graph.
#
# WHAT "PURE RUST" MEANS HERE — read this before trusting the filename
# ---------------------------------------------------------------------
# Every TLS user in this workspace is meant to resolve to **rustls with the
# `ring` provider**.
#
# `ring` is NOT literally pure Rust: it ships some C and pre-generated assembly
# compiled through `cc`. So the honest invariant is narrower than the script's
# name suggests, and worth stating precisely rather than leaving a comment that
# the next person discovers is false:
#
#   * no cmake
#   * no system TLS library (no OpenSSL, no platform TLS stack as the provider)
#   * no build-time dependency beyond the C toolchain every target already has
#
# `aws-lc-rs` — rustls' other provider — is strictly worse on exactly that axis:
# it wants cmake and a fuller C build environment. That is the whole reason the
# feature selection exists, and what this gate protects.
#
# The practical payoff: `host/` cross-compiles clean for Windows and macOS, and
# the daemon's default build pulls in no system C *libraries*.
#
# Nothing enforced it. `host.yml` runs fmt/clippy/build/test and no dependency
# check at all; the only `ldd` assertion lives in `rust.yml`, applies to the
# daemon binary alone, and greps specifically for `libcec|libp8-platform` — it
# would sail straight past a crypto-provider regression.
#
# The invariant is one careless default-feature away at all times. rumqttc is
# the live example: its DEFAULT feature enables `tokio-rustls/default`, whose
# provider is `aws-lc-rs` (cmake + a C toolchain). It is pulled in only via
# `default-features = false, features = ["use-rustls-no-provider"]` plus a
# direct `rustls` pinned to `ring`. A single `cargo update`, a transitive
# default-feature change, or one future contributor adding a TLS dependency
# re-introduces it silently — and the first symptom is a broken Windows build,
# far from the cause.
#
# SCOPE — read this before assuming it checks more than it does.
# This asserts the TLS/crypto provider only. It says nothing about the daemon's
# `cec` feature, which deliberately static-links libcec and needs libudev; that
# has its own `ldd` gate in `rust.yml`.
#
# NOT BANNED, on purpose: `schannel` (Windows) and `security-framework` (macOS)
# are platform trust-store bindings that `rustls-native-certs` legitimately
# uses. They bind OS system libraries that are always present and need no build
# toolchain, so they do not threaten the cross-compile the way `aws-lc-rs` does.
#
# USAGE
#   scripts/assert-pure-rust-tls.sh [extra cargo tree args...]
#   scripts/assert-pure-rust-tls.sh --workspace
#   scripts/assert-pure-rust-tls.sh -p tv-shell-input --features cec,mcp
#
# Run it per-platform: the resolved graph is target-dependent, so the Windows
# leg is the one that would actually catch a Windows-only regression.

set -euo pipefail

# `cargo tree -i <crate>` exits 0 when the crate IS in the graph, so the check
# is inverted: success is the failure condition.
BANNED=(
  aws-lc-rs   # rustls' other provider: wants cmake + a fuller C build env
  aws-lc-sys  # its -sys half, in case only the lower crate is pulled
  native-tls  # would bind OpenSSL/Schannel/Security.framework as a TLS stack
  openssl-sys # any path to system OpenSSL at all
)

args=("$@")
if [ ${#args[@]} -eq 0 ]; then
  args=(--workspace)
fi

echo "Asserting no cmake/system-TLS crypto crate in: cargo tree ${args[*]}"

failed=0
for crate in "${BANNED[@]}"; do
  # Capture combined output so a genuine cargo failure can be told apart from a
  # clean "not in the graph". Without that distinction a broken manifest or a
  # network error would ALSO exit non-zero and the gate would silently pass —
  # a check that cannot fail is not a check.
  if out=$(cargo tree "${args[@]}" --invert "$crate" 2>&1); then
    echo "::error::banned crate '$crate' is in the dependency graph"
    printf '%s\n' "$out" | head -30
    failed=1
  elif printf '%s' "$out" | grep -q 'did not match any packages'; then
    echo "  ok: $crate absent"
  else
    echo "::error::cargo tree failed unexpectedly while checking '$crate' —" \
         "treating this as a failure rather than a pass"
    printf '%s\n' "$out"
    exit 1
  fi
done

if [ "$failed" -ne 0 ]; then
  cat >&2 <<'EOF'

A cmake/system-TLS crypto crate reached the dependency graph.

This breaks the invariant that lets host/ cross-compile for Windows and macOS
without cmake or a system TLS library. The usual cause is a dependency pulling
rustls through its DEFAULT features, which select the aws-lc-rs provider.

Fix it at the dependency, not here — for example:

  somecrate = { version = "x", default-features = false, features = ["use-rustls-no-provider"] }
  rustls    = { version = "0.23", default-features = false, features = ["ring", "std", "tls12", "logging"] }

Then re-run:  scripts/assert-pure-rust-tls.sh --workspace
EOF
  exit 1
fi

echo "OK: TLS/crypto graph resolves to rustls + ring (no cmake, no system TLS)."
