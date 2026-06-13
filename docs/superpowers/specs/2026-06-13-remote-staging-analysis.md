# Remote staging ("polite to the network") - analysis and plan

Date: 2026-06-13
Scope: `src-tauri/src/remote.rs`, `src-tauri/src/staging.rs`, `src-tauri/src/encoder.rs`
(wave loop + `encode_single_file`).

## 1. The concept

Transcoding straight off a network share is bad for two reasons:

1. **The encoder stalls.** ffmpeg reads a file non-linearly (header/moov seeks, packet-by-packet
   demux, two-pass reads the file twice). Over a high-latency share each of those reads blocks,
   so a fast local encoder spends much of its time waiting on the network.
2. **The network suffers.** Those reads are spread across the entire, slow duration of the
   encode, so the share is under sustained latency-sensitive load for a long time, hurting
   everyone else using it.

Staging converts that long, bursty, latency-sensitive access into **one fast sequential bulk
copy**. The encoder then runs at full local speed, and the network sees a single short burst
instead of hours of dribble. The idea is sound and worth keeping.

## 2. What the code actually does (verified)

- **Detection** (`remote.rs`): per-platform mount-table parsing (Linux `/proc/mounts`, macOS
  `mount`, Windows `GetDriveTypeW` + UNC). Longest-prefix match, autofs filtered, octal-escape
  unescaping, a 5-second `canonicalize` timeout to survive dead mounts, and a directory-level
  canonicalise cache. This part is solid and well tested.
- **Input staging** (`staging.rs`, `encoder.rs:2512-2555`): remote input files are bulk-copied
  to a local staging dir in **waves**. A wave is a run of consecutive remote files sized to fit
  ~90% of free staging space (`WavePlanner`). The whole wave is staged, then the whole wave is
  encoded, then the whole wave is cleaned up. Local files between remote files are encoded in
  place. Staging uses async `tokio::fs::copy`, has a 1.1x free-space pre-check, and a `Drop`
  guard so staged copies are removed even on panic/SIGINT.
- **Output writing**:
  - `replace` / `beside` mode (`encoder.rs:2616-2727`): because the input path was rewritten to
    the staged **local** copy, ffmpeg writes its output **locally** too, and the wave-cleanup
    step copies it back to the remote location with a safe backup-rename-delete pattern.
  - `folder` mode (the default) (`encoder.rs:1495-1500`): output is written **directly to
    `settings.output_folder`**. If that folder is on a remote mount, the encoded file streams
    over the network for the entire encode. **This is a real gap** - the politeness concept is
    not applied to output in the most common mode.

## 3. Strengths

- Detection is robust and cross-platform.
- Input staging is the high-value half and it is done well (sequential bulk copy, disk-aware,
  crash-safe cleanup).
- `replace`/`beside` already stage output correctly and copy back atomically.

## 4. Is staging the best implementation of the concept? Gaps.

Staging is the right core idea, but the current shape leaves value on the table:

### 4.1 Output is only half-staged
As above: `folder` mode writes output straight to a (possibly remote) folder. The concept says
"don't do sustained I/O over the network during the encode", but folder-mode output does exactly
that. See the shipped fix in section 6.

### 4.2 The wave model serialises network and CPU
A wave is *fully staged, then fully encoded, then cleaned up*. Consequences:

- While a wave stages, the **CPU is idle** (no encoding yet).
- While a wave encodes, the **network is idle** (nothing staging).
- The first encode cannot start until the **entire wave** has copied. On a slow link that is a
  long dead wait before any progress.

So the wall-clock is roughly `sum(stage) + sum(encode)` when it could approach
`max(sum(stage), sum(encode))`. The fix is a **prefetch-one-ahead pipeline**: stage file *N+1*
while encoding file *N*. The encoder never waits for the network after the first file, and the
network only ever holds ~2 files locally at once (strictly less disk than the current
whole-wave-on-disk approach). This is the single biggest improvement available, but it partly
replaces the wave model and adds concurrency around destructive cleanup, so it is treated as a
designed follow-up rather than a drive-by change (section 7).

### 4.3 Planning only considers input remoteness, not output
`WavePlanner` keys entirely off whether each **input** is remote. A batch with **local inputs**
but a **remote output folder** gets no staging at all, even though the output writes would
benefit from it. Fully closing 4.1 in every case means making planning output-aware (section 7).

### 4.4 "Polite" and "fast for the encoder" are conflated
The feature is justified as politeness but is actually tuned for encoder throughput (stage as
fast as possible). Genuine politeness on a shared/metered link might want an **optional copy
bandwidth cap** (e.g. `--remote-bandwidth-limit 50M`) or off-peak scheduling. Minor, optional.

### 4.5 Detection is binary
A gigabit LAN NAS and a 5 Mbps WAN sshfs are treated identically. A quick throughput probe could
skip staging when the share is already faster than the encoder can consume, but a one-off probe
is unreliable and the safe default (always stage remote) is fine. Low priority.

## 5. Verdict

Keep staging. It is the correct concept and the input half is well built. The implementation is
**not** the best version of the concept yet because of 4.1 (output not staged in folder mode)
and 4.2 (no pipelining). Closing 4.1 for the common case is contained and safe and is shipped
now. 4.2 and 4.3 are the high-value architectural follow-ups, designed below.

## 6. Shipped now: stage folder-mode output within remote-input waves

**Goal:** when a wave already stages remote inputs *and* output mode is `folder` *and* the output
folder is on a remote mount, write the encode output locally and bulk-copy it back, mirroring the
existing `replace`/`beside` behaviour. Behaviour is **identical to today** unless all three
conditions hold, so risk is contained.

**Design:**
- `encode_single_file` gains `output_dir_override: Option<&Path>`. In the `folder` branch the
  effective output folder is `output_dir_override.unwrap_or(&settings.output_folder)`.
- Before encoding a wave that has a `staging_dir`, compute `output_folder_remote` once
  (`MountCache::is_remote(settings.output_folder)`), gated on folder mode and staging being
  enabled. If true, the per-file override is `staging_dir/folder-output/`.
- In wave cleanup, for folder mode with the override active and the item Done, copy
  `staging_dir/folder-output/{base}.{ext}` to `settings.output_folder/{base}.{ext}` (creating the
  remote dir), then delete the local copy. Reuse the existing `resolve_file_settings` extension
  logic already used by the beside back-copy.

**Not covered (documented, see 4.3):** local-input + remote-output batches still write output
directly, because they never enter a staging wave. Closing that needs output-aware planning (7.2).

## 7. Designed follow-ups (deferred - need their own review)

These touch concurrency and/or planning around code that deletes and replaces source files, so
they are specified here for a focused, separately-reviewed change rather than rushed.

### 7.1 Prefetch-one-ahead staging pipeline
Replace "stage whole wave, then encode whole wave" with a per-file pipeline:
```
stage(file[0])
for i in 0..N:
    if i+1 < N: spawn_background stage(file[i+1])    # bounded by free space for 2 files
    encode(file[i])
    back-copy + cleanup(file[i])
    await the spawned stage(file[i+1])
```
- Disk: needs room for 2 staged files, which is **less** than the current whole-wave footprint.
- Cancellation: an in-flight prefetch task must be awaited/aborted and its partial copy removed on
  cancel (the `StagingContext` `Drop` guard already handles partials).
- Wall-clock approaches `max(sum(stage), sum(encode))` instead of the sum.
- Tests: the planner/budget decisions are unit-testable; the async interleaving needs an
  integration test with a synthetic slow-copy.

### 7.2 Output-aware wave planning
Teach `WavePlanner` that a file needs a staging wave if **its input is remote OR the output
folder is remote**. This closes 4.1 for local-input/remote-output batches and unifies the output
back-copy path across all three output modes.

### 7.3 Optional copy bandwidth cap
`--remote-bandwidth-limit <RATE>` (and a GUI field) to throttle staging copies for genuinely
polite behaviour on shared/metered links. Implement as a chunked copy with a token-bucket rate
limiter instead of `tokio::fs::copy`.

## 8. Post-QA notes (code-auditor)

A code-audit of the shipped change surfaced two things worth recording:

- **DV Tier-1 extension mismatch (pre-existing).** The wave-cleanup back-copy resolves the output
  extension with `resolve_file_settings`, but `encode_single_file` overrides the extension to
  `mp4` for Dolby Vision Tier-1 sources *after* that call. A DV source in a non-MP4 container
  could therefore have its encoded `.mp4` looked up under the wrong name and left in staging. The
  **new folder-mode block applies the DV override** when resolving the filename; the **identical
  latent bug still exists in the pre-existing `replace`/`beside` back-copy** (lines ~2660-2666)
  and is left untouched here to keep the change contained. Recommended follow-up: extract a single
  `resolve_output_ext(item, settings, preserve_hdr, encoders)` helper used by `encode_single_file`
  and all three back-copy paths so the rule cannot diverge again.
- **Staging disk budget excludes outputs (pre-existing, all modes).** `WavePlanner` sizes a wave
  against input sizes only; outputs also land in the staging dir during the wave. For encodes that
  target a higher bitrate than the source this can overfill the staging partition. This is the same
  for `replace`/`beside` today; documented as a known limitation, to be addressed alongside the
  prefetch/output-aware-planning work in 7.1/7.2.

**Build/test status:** this host has no Rust toolchain, so the change is hand-reviewed and
compile-reasoned (code-auditor: "compiles cleanly") but **not** built or run here. Run
`cargo build --manifest-path src-tauri/Cargo.toml --no-default-features --features cli` and
`cargo test ... --features cli` before relying on it.
