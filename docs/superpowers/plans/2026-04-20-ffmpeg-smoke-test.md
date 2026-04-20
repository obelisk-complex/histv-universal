# FFmpeg CI Smoke Test Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Gate the Flatpak build and Linux full-bundle build on a fast FFmpeg validation job that catches 404s, sha256 mismatches, and missing codecs before expensive build steps run.

**Architecture:** Two workflow files change. `build-flatpak.yml` gets a new `validate-ffmpeg` job whose steps parse the Flatpak manifest for the URL and sha256, download the binary, and run x265/x264 smoke encodes; the existing `build` job gains `needs: validate-ffmpeg`. `build-platform.yml` gets its three stale Linux FFmpeg env vars updated, plus an inline smoke-test step (Linux only) inserted after the existing download step.

**Tech Stack:** GitHub Actions YAML, Python 3 (pre-installed on ubuntu-22.04 runners), FFmpeg (BtbN static build), bash.

---

## Files

| Action | Path |
|--------|------|
| Modify | `.github/workflows/build-flatpak.yml` |
| Modify | `.github/workflows/build-platform.yml` |

---

### Task 1: Add `validate-ffmpeg` gate job to `build-flatpak.yml`

**Files:**
- Modify: `.github/workflows/build-flatpak.yml`

- [ ] **Step 1: Insert the `validate-ffmpeg` job**

Open `.github/workflows/build-flatpak.yml`. The `jobs:` block currently contains only `build:`. Insert the new job **before** `build:`:

```yaml
jobs:
  validate-ffmpeg:
    runs-on: ubuntu-22.04
    steps:
      - name: Checkout
        uses: actions/checkout@de0fac2e4500dabe0009e67214ff5f5447ce83dd # v6.0.2
        with:
          ref: ${{ github.event.inputs.ref || github.ref }}

      - name: Extract FFmpeg URL and sha256
        id: ffmpeg-meta
        run: |
          python3 << 'PYEOF'
          import yaml, os
          data = yaml.safe_load(open('com.histv.encoder.yml'))
          mod = next(m for m in data['modules'] if m['name'] == 'ffmpeg')
          src = next(s for s in mod['sources'] if s['type'] == 'archive')
          with open(os.environ['GITHUB_OUTPUT'], 'a') as fh:
              fh.write(f"url={src['url']}\n")
              fh.write(f"sha256={src['sha256']}\n")
          PYEOF

      - name: Download and verify FFmpeg
        run: |
          set -e
          curl -fSL --retry 3 -o ffmpeg.tar.xz "${{ steps.ffmpeg-meta.outputs.url }}"
          echo "${{ steps.ffmpeg-meta.outputs.sha256 }}  ffmpeg.tar.xz" | sha256sum -c -
          mkdir -p ffmpeg-extract ffmpeg-bin
          tar xf ffmpeg.tar.xz --strip-components=1 -C ffmpeg-extract
          FFMPEG=$(find ffmpeg-extract -name "ffmpeg" -not -name "ffprobe" -type f | head -1)
          cp "$FFMPEG" ffmpeg-bin/ffmpeg
          chmod +x ffmpeg-bin/ffmpeg

      - name: Smoke-test FFmpeg
        run: |
          set -e
          ffmpeg-bin/ffmpeg -version
          ffmpeg-bin/ffmpeg -f lavfi -i testsrc2=size=192x108:rate=1 \
            -vframes 5 -c:v libx265 -preset ultrafast /tmp/smoke_hevc.mkv
          test -s /tmp/smoke_hevc.mkv
          ffmpeg-bin/ffmpeg -f lavfi -i testsrc2=size=192x108:rate=1 \
            -vframes 5 -c:v libx264 -preset ultrafast /tmp/smoke_h264.mp4
          test -s /tmp/smoke_h264.mp4

  build:
    needs: validate-ffmpeg
    runs-on: ubuntu-22.04
    # ... rest of existing build job unchanged
```

The only change to the existing `build:` job is adding `needs: validate-ffmpeg` as the second line (after `build:`).

- [ ] **Step 2: Validate YAML syntax**

```bash
python3 -c "import yaml; yaml.safe_load(open('.github/workflows/build-flatpak.yml'))" && echo OK
```

Expected output: `OK`

- [ ] **Step 3: Commit**

```bash
git add .github/workflows/build-flatpak.yml
git commit -m "ci(flatpak): gate build on FFmpeg download and smoke test"
```

---

### Task 2: Fix stale FFmpeg env vars in `build-platform.yml`

**Files:**
- Modify: `.github/workflows/build-platform.yml` (lines 37-39)

- [ ] **Step 1: Replace the three stale env vars**

In the `env:` block at the top of `build-platform.yml`, replace:

```yaml
  FFMPEG_BTBN_TAG: "autobuild-2026-04-01-13-13"
  FFMPEG_BTBN_LINUX: "ffmpeg-n8.1-7-ga3475e2554-linux64-gpl-8.1.tar.xz"
  FFMPEG_BTBN_LINUX_SHA256: "57417a11d21fc9ec76b4a250f95754d99d0f272765ba62b2d0aaf87d02c32cd8"
```

With:

```yaml
  FFMPEG_BTBN_TAG: "latest"
  FFMPEG_BTBN_LINUX: "ffmpeg-n8.1-latest-linux64-gpl-8.1.tar.xz"
  FFMPEG_BTBN_LINUX_SHA256: "8e1943fbc5b2e4950b1f49047b20cd4fe86002305f2b127abad6c6e5f2d0c909"
```

- [ ] **Step 2: Validate YAML syntax**

```bash
python3 -c "import yaml; yaml.safe_load(open('.github/workflows/build-platform.yml'))" && echo OK
```

Expected output: `OK`

- [ ] **Step 3: Commit**

```bash
git add .github/workflows/build-platform.yml
git commit -m "ci(platform): update Linux FFmpeg URL to BtbN latest n8.1 build"
```

---

### Task 3: Add inline smoke-test step to `build-platform.yml`

**Files:**
- Modify: `.github/workflows/build-platform.yml`

- [ ] **Step 1: Insert smoke-test step after "Download ffmpeg"**

In `build-platform.yml`, find the step named `Download ffmpeg` (around line 160). Insert a new step **immediately after** it (before the `# ── Build -full variant bundles` comment):

```yaml
      - name: Smoke-test FFmpeg (Linux)
        if: inputs.label == 'linux'
        shell: bash
        run: |
          set -e
          ffmpeg-bin/ffmpeg -version
          ffmpeg-bin/ffmpeg -f lavfi -i testsrc2=size=192x108:rate=1 \
            -vframes 5 -c:v libx265 -preset ultrafast /tmp/smoke_hevc.mkv
          test -s /tmp/smoke_hevc.mkv
          ffmpeg-bin/ffmpeg -f lavfi -i testsrc2=size=192x108:rate=1 \
            -vframes 5 -c:v libx264 -preset ultrafast /tmp/smoke_h264.mp4
          test -s /tmp/smoke_h264.mp4
```

Note: `ffmpeg-bin/ffmpeg` is the flat path where the Linux download step copies the binary (see the `cp "$FFMPEG" ffmpeg-bin/ffmpeg` line in the existing "Download ffmpeg" step). macOS and Windows labels skip this step via the `if:` guard.

- [ ] **Step 2: Validate YAML syntax**

```bash
python3 -c "import yaml; yaml.safe_load(open('.github/workflows/build-platform.yml'))" && echo OK
```

Expected output: `OK`

- [ ] **Step 3: Commit**

```bash
git add .github/workflows/build-platform.yml
git commit -m "ci(platform): smoke-test bundled FFmpeg before full-bundle steps"
```

---

### Task 4: Push and verify

- [ ] **Step 1: Push to main**

```bash
git push origin main
```

- [ ] **Step 2: Trigger a Flatpak workflow_dispatch to exercise the gate**

```bash
gh workflow run build-flatpak.yml --repo obelisk-complex/histv-universal --ref main
```

- [ ] **Step 3: Watch the run**

```bash
gh run watch --repo obelisk-complex/histv-universal
```

Expected: `validate-ffmpeg` job completes successfully, then `build` job starts and completes. Both green.

- [ ] **Step 4: Confirm job ordering in the run summary**

```bash
gh run list --repo obelisk-complex/histv-universal --limit 3
```

The manually triggered `Build Flatpak` run should show `completed success`.
