# Release Checklist — phi-agent-tui

The family release runbook for this crate. Follow the steps in order. (Ops
lessons from prior family releases: long scripts log to disk and resume by
slicing, `cargo publish` retries are single-shot with `CARGO_HTTP_TIMEOUT=600`,
never re-run a release script wholesale after a partial run.)

## Pre-release

### 1. Gates — everything green before anything is tagged

```bash
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --document-private-items

# Coverage gate (design §7.3 caliber: llvm-cov; TTY glue excluded; the
# exclusion surface is set by measured data).
cargo llvm-cov --lib --ignore-filename-regex '(^|/)run\.rs$' --fail-under-lines 80
```

Coverage caliber: `run.rs` is the event loop / terminal glue — it is
end-to-end covered by the PTY smoke gate below, not by unit tests, so it is
excluded from the unit-coverage number. Measured 2026-11-02: **89.53% lines**
on the non-glue surface (5,290 lines / 554 missed); unfiltered total 75.09%.

PTY smoke gate (the release-gate asset, lives in the reference consumer):
phimint `scripts/pty_smoke/smoke.py` — **51 assertions** across 10 scenarios
surfaces / fixed protocol / periodic probes / the shallow-tier injection
timing invariant.

### 2. Verify dependent crates are published (dependency order)

```bash
# agent-base → agent-works → phi-tools → phi-telemetry → log-core
#   → phi-agent → phi-tui → phi-kernel-tools → phi-agent-tui
cargo update
cargo check   # must resolve every dep from crates.io
```

Version requirements are 0.x caret: if a dependency's minor was bumped in
this cycle, republish every crate whose requirement points at it in the same
run, in dependency order (this crate last).

### 3. Name check (first publish only)

```bash
curl -s -A "release-check" https://crates.io/api/v1/crates/phi-agent-tui
# 404 {"errors":[{"detail":"crate `phi-agent-tui` does not exist"}]} = free
```

`phi-agent-tui` was free as of 2026-11-02 (404).

### 4. Update CHANGELOG

- [ ] All notable changes under the new version heading
- [ ] Breaking changes in a `### Breaking` section
- [ ] Comparison links at the bottom updated

### 5. Update version

- [ ] `Cargo.toml`: `version = "X.Y.Z"`
- [ ] `README.md` / `README_CN.md`: version badges / install snippets

### 6. Release shape clean

```bash
grep -rn "path =" Cargo.toml       # must return nothing (pure version deps)
grep -rn "// TODO\|// FIXME" src/  # must return nothing (unfinished-work
                                   # markers; test fixtures may carry the bare
                                   # words — those are data, not debt)
```

## Tag and release

### 7. Git tag

```bash
git tag -a v0.1.0 -m "v0.1.0"
git push origin main --tags
```

### 8. CI must pass BEFORE publishing

The CI workflow runs fmt/clippy/test on ubuntu + macos + **windows** and the
coverage gate. The platform matrix is the compile check that keeps a
platform-specific compile error off crates.io — a `cargo publish` that
precedes it has to be yanked and republished (family incident: phimint 0.7.0,
Windows-only build break). Wait for green.

### 9. Publish

```bash
CARGO_HTTP_TIMEOUT=600 cargo publish
```

Single-shot retry on upload timeout only — never re-run surrounding release
script after the tag is pushed (guards will trip on the duplicate).

### 10. Post-release: restore local dev state

Re-add the uncommitted `[patch.crates-io]` local path overrides across the
whole family (capability packs included) in one pass — the published shape is
version-only, the working tree is path-based.

## Semver policy

| Change | Version bump |
|--------|-------------|
| Bug fix (no API change) | Patch |
| New field/slot with a default, backward-compatible | Minor |
| Field removed or meaning changed | Major (0.x: minor) |
