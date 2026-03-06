# Honey, I Shrunk The Vids v1.0.7 - Patch Notes

## New Features

### QP / CRF rate control toggle
- Software encoders (libx265, libx264) now offer a choice between **QP** (Quantisation Parameter) and **CRF** (Constant Rate Factor) for quality-mode encodes.
- A segmented toggle appears on the Encoding Settings tab when a software encoder is selected.
- **QP** uses a fixed quantiser per frame - output size is less predictable but quality is frame-consistent.
- **CRF** lets the encoder vary quantisation to hit a perceptual quality target - generally preferred for software encoding.
- Hardware encoders continue to use QP only (CRF is not supported by HW encoder APIs); the toggle is hidden when a HW encoder is selected.
- The file queue's "Target Bitrate" column reflects the active mode (e.g. `CRF 20` vs `CQP (20/22)`).

### Output next to input file
- A new **"Put output next to input file"** checkbox in Output Settings.
- When enabled, an `output` subfolder is created alongside each input file and the encoded file is saved there.
- This overrides the manual output folder path; the output folder text box and Browse button are greyed out while the option is active.

### Tooltips on all settings
- Every setting in both the Encoding Settings and Output Settings tabs now has a **`?` tooltip icon** that explains what the option does on hover.
- Tooltips cover codec choice, target bitrate behaviour, QP vs CRF, pixel format, HDR, audio codec, audio cap, output folder, container format, overwrite/delete-source behaviour, logging, notifications, and post-batch actions.

## Improvements

### Settings layout redesign
- Section headers now have a subtle bottom border separator for clearer visual grouping.
- Label widths are consistent across both tabs (150px minimum).
- Checkbox rows have uniform minimum heights for a tidier vertical rhythm.
- The codec and encoder dropdowns now share a single row more cleanly.
- Overall spacing and alignment improved across both settings panels.

## Backend Changes
- Added `crf_flags()` function to `encoder.rs` - emits `-preset slow -crf <value>` for libx265/libx264, with automatic fallback to `cqp_flags()` for hardware encoders.
- `start_batch_encode` now parses `rateControlMode`, `crf`, and `outputNextToInput` from the frontend settings.
- `AppConfig` struct in `config.rs` extended with `crf: u32`, `rate_control_mode: String`, and `output_next_to_input: bool` fields (with backwards-compatible defaults).
