# HISTV iOS Port Plan

**Status: PARKED.** No implementation has started. This document locks decisions made during a scoping session on 2026-05-02 so a future session can resume from a known baseline.

Cross-reference: `llm-wiki/wiki/concepts/histv-ios-port-plan-2026-05-02.md`.

## Goal

Native iOS companion app to the Tauri+Rust desktop transcoder. On-device encode of short or small videos chosen by the user. Not a desktop replacement.

## Target Use Case

HISTV mobile is not for shrinking TV shows or films. Those live inside DRM-encrypted streaming app sandboxes that iOS does not let HISTV touch. Two real use cases:

1. **Shrink-to-share**: user took a video on their phone (4K HEVC, often big) and wants to send it to someone. Source is in the Photos library or share-sheet input. Output goes back via share-sheet to Messages, Mail, WhatsApp, etc.
2. **Shrink-to-store**: user is running out of phone storage. Wants to re-encode their own captured videos in place (or alongside) at smaller size, without deleting the originals.

Implications:

- Single-file or small-batch is the dominant interaction. The desktop's "drop a season folder" workflow is irrelevant on phone.
- Source is almost always Photos library or share-sheet input; Document Picker is a secondary path.
- Output destinations: replace in Photos, save alongside in Photos, or share-sheet export. No "save next to source on disk" path because the Photos library is opaque.
- Phone-captured H.264/HEVC at SDR Rec.709 is the dominant input. HDR phone capture exists (Dolby Vision iPhones since iPhone 12); tonemap to SDR per locked policy.
- Encode times stay short because clips are short. Thermal pressure is bounded by the use case itself.

## Controls

- **Quality slider**: single user-facing control mapped internally to a QP/CRF range. Sane endpoints (e.g. "small file" to "near-lossless"); no exposed numerical values by default.
- **Fast / Quality toggle**: when VideoToolbox H.264 hardware encode is available, "Fast" uses GPU; "Quality" uses software for better rate-distortion at the same target. When GPU unavailable, the toggle hides or locks to software.
- No exposed bitrate, codec, container, audio-codec, or HDR controls. The locked H.264/MP4/AAC/SDR scope removes the need.

## Scope (Locked)

- Output container: MP4.
- Output video codec: H.264 via VideoToolbox hardware encode. Always.
- Output audio codec: AAC via AudioToolbox / AVFoundation.
- HDR handling: tonemap to SDR. Always.
- Input: Document Picker (multi-select) and Share Sheet.
- Output destinations: replace source, save next to source, or share-sheet export.
- Foreground-only encode with explicit "keep app open" UX.
- Self-throttling on thermal pressure to protect the device.
- Distribution: App Store, $99/yr Apple Developer cert, privacy manifest, export-compliance paperwork.

## Out of Scope

- No CLI version.
- No format negotiation, no per-file codec resolution, no precision mode, no preserve-AV1, no compatibility-mode toggle (compatibility is the only mode).
- No AV1 or HEVC output.
- No HDR preservation, DV preservation, HDR10 passthrough, or HDR10+.
- No deep repair or mediainfo introspection.
- No MP4Box, mkvmerge, x264, x265 (GPL incompatible with App Store).
- No FFmpeg CLI spawning (sandbox forbids).
- No SMB or NFS mounts.
- No "scan a folder" workflow (sandbox).
- No Sonarr or Radarr on device. (A remote-companion path could carry this; that is a separate plan.)
- No background completion promise.

## Engine

Rewrite required. Reuse the existing Rust core opportunistically where it cross-compiles for iOS (cargo + lipo + an Objective-C bridge), but treat the encode pipeline as green-field. Encode goes through Apple AVFoundation and VideoToolbox APIs only.

## Codec

- Video: H.264, VideoToolbox hardware encode.
- Audio: AAC, AudioToolbox / AVFoundation.
- Container: MP4.
- HDR: tonemap to SDR on ingest.
- Zero GPL components shipped.

## I/O

- Input pickers: Document Picker (multi-file) and Share Sheet.
- Output: in-place replace, save adjacent, or share-sheet export.
- No directory scan, no network mounts.

## Thermals

Self-throttle CPU and encode pace. Slower is acceptable; thermal damage and bad reviews are not. Concrete throttling thresholds and backoff curve are deferred (see Open Questions).

## Background

Foreground-only. Show explicit "keep the app open" UX. Do not promise background completion.

## Licensing

- H.264 royalty pool: assumed covered by Apple's MPEG-LA umbrella for app distribution. Verify current 2026 status before shipping.
- AAC: assumed similarly covered. Verify.
- No GPL bundling, by construction.

## Distribution

App Store. $99/yr Apple Developer cert. Privacy manifest plus export-compliance self-classification required.

## Open Questions

Do not pick answers in this document.

1. UI shell stack: native SwiftUI (best feel, full rewrite) vs Tauri Mobile (alpha, shared codebase, risky) vs Capacitor + WebView (reuse current HTML/JS/CSS, weaker feel). Decision deferred until the desktop UI redesign settles, since shared CSS and layout are the most reusable artefact.
2. Concrete thermal throttling thresholds. Pause when `ProcessInfo.thermalState` hits `serious`? Backoff curve shape?
3. Foreground keepalive UX. How aggressive should the prompt be? Use the AVAudioSession trick to prevent screen sleep?
4. Batch resume after app kill or crash. Persist queue and per-file progress? Resume mid-file via keyframe seek, or restart the file?
5. Audio re-encode policy when the source is not AAC. Always re-encode? Bitrate cap?
6. Encryption export filing. Annual self-classification needed (TLS for any update check counts).
7. Privacy manifest entries. Anything to declare? File-access usage strings.
8. iPad differentiation. Same app with larger layouts, or iPad-specific affordances (split view, multi-window)?
9. Test matrix. Minimum supported devices? iPhone SE 3rd gen (A15) is a sane floor.
10. Remote-companion path (Option B from the brainstorm) is a separate plan. The on-device port and the remote companion could ship as one app with a mode switch; that decision is parked.
11. Quality-slider endpoint mapping. Which QP/CRF values feel "small" vs "near-lossless" on phone-captured content? Probably needs a one-off perceptual study.
12. Fast/Quality toggle default. Fast (battery, speed) or Quality (smaller file)? Probably Fast, matching the Drop/Trust/Glance ethos and the shrink-to-share use case.
13. Photos library write-back. Replace-in-place is destructive; iOS Photos has an undo window for edits, but a full re-encode is treated as a new asset by some flows. Verify behaviour and surface honestly to the user.
14. Share-sheet input chain. If a user shares from Messages to HISTV to Messages, is the round-trip intuitive enough? Test the flow.

## References

- Desktop project root: `/media/owner/Workspace/histv-universal/`
- Wiki page: `llm-wiki/wiki/concepts/histv-ios-port-plan-2026-05-02.md`
- Apple: VideoToolbox, AVFoundation, AudioToolbox, ProcessInfo.thermalState, App Store Review Guidelines, Privacy Manifest reference.
