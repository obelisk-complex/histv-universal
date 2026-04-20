# FFmpeg CI Smoke Test — Design Spec

**Date:** 2026-04-20
**Status:** Approved

## Problem

The v2.6.0 release failed because the BtbN FFmpeg autobuild URL pinned in
`com.histv.encoder.yml` and `build-platform.yml` no longer existed (404).
The Flatpak build and the Linux full-bundle build both download FFmpeg from
external URLs; neither workflow verified the binary before spending build time.

## Goal

Add a fast gate that validates the bundled FFmpeg binary (download, sha256,
and basic encode capability) before any expensive build step runs.

## Scope

Three files change:

- `.github/workflows/build-flatpak.yml` — new `validate-ffmpeg` gate job
- `.github/workflows/build-platform.yml` — fix stale URL env vars + inline smoke step
- `com.histv.encoder.yml` — already updated (URL + sha256 fixed in the hotfix commit)

## Design

### build-flatpak.yml — gate job

A new `validate-ffmpeg` job is added. The existing `build` job gains
`needs: validate-ffmpeg`.

Steps:
1. Checkout
2. Parse `com.histv.encoder.yml` with `python3` to extract the FFmpeg `url`
   and `sha256` from the `ffmpeg` module's archive source. No hardcoding —
   the gate stays in sync with the manifest automatically.
3. Download the binary with `curl -fSL --retry 3`, verify sha256.
4. Extract and smoke-test:
   - `ffmpeg -version` (confirms the binary runs and prints codec list)
   - Encode 5 frames of a synthetic `testsrc2` signal to x265 → assert output non-empty
   - Encode 5 frames of the same signal to x264 → assert output non-empty
5. Fail fast on any error; Flatpak build never starts.

Runner: `ubuntu-22.04` (same as the build job). `python3-yaml` is available
on the default runner image.

### build-platform.yml — env var fix + inline smoke

**Env var update** (top-of-file):

| Variable | Old value | New value |
|---|---|---|
| `FFMPEG_BTBN_TAG` | `autobuild-2026-04-01-13-13` | `latest` |
| `FFMPEG_BTBN_LINUX` | `ffmpeg-n8.1-7-ga3475e2554-linux64-gpl-8.1.tar.xz` | `ffmpeg-n8.1-latest-linux64-gpl-8.1.tar.xz` |
| `FFMPEG_BTBN_LINUX_SHA256` | `57417a11d...` | `8e1943fbc...` |

**New step** inserted immediately after "Download ffmpeg", guarded by
`if: inputs.label == 'linux'`. Same two smoke encodes as the gate job.
The full-bundle steps (AppImage repack, CLI tarball) only run if this passes.

### Smoke-test parameters

- Input: `testsrc2=size=192x108:rate=1`, 5 frames (fast, deterministic)
- x265: `-preset ultrafast` (no quality requirements, just codec availability)
- x264: `-preset ultrafast`
- Assertion: `test -s <output>` (non-empty file)

These parameters exercise the codecs histv depends on without requiring a
real source file or real encoding quality.

## Out of scope

- macOS and Windows FFmpeg sources (Evermeet / Gyan) — different suppliers,
  different failure modes; add separately if needed.
- Periodic sha256 refresh automation — the sha256 in the manifest and
  build-platform.yml will go stale when BtbN cuts a new n8.1 build. A
  scheduled workflow to detect and PR the update is a separate task.
- Flathub submission — the Flatpak is CI-only; using `latest` is acceptable
  here but would need a pinned commit for Flathub.
