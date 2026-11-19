# Creating Themes

Honey, I Shrunk The Vids supports custom themes via simple JSON files. You pick 6 colours and the app derives everything else - borders, washes, status colours, progress bars - automatically.

## Built-in Doublet themes

The app's look follows the Doublet design system. Four dark Doublet pairings head the Theme dropdown: Doublet (blue thread on blue-black), Doublet Dusk, Doublet Ember and Doublet Filigree. They are built into the app and have no JSON file.

Your own themes use the same six-colour format and appear below them. A theme file named "Doublet", "Doublet Dusk", "Doublet Ember" or "Doublet Filigree" replaces the built-in pairing of that name.

## Where to put themes

Theme files live in a `themes/` folder next to the application executable. On first launch the app creates this folder and writes the built-in JSON themes into it. Any `.json` file you drop in there will appear in the Theme dropdown.

## Theme file format

A theme file is a JSON object with a `name` and a `colors` object containing 6 colour values:

```json
{
  "name": "My Theme",
  "colors": {
    "background": "#101014",
    "surface": "#24242B",
    "text": "#E6E6EA",
    "primary": "#0A6CFF",
    "success": "#22e07a",
    "error": "#ff5a5a"
  }
}
```

That's all you need. Save it as a `.json` file in the `themes/` folder, restart the app, and it appears in the dropdown.

## What each colour controls

| Key | What it does |
|-----|-------------|
| `background` | The ground: main app background, queue, table header |
| `surface` | Raised panels: quality rail, status strip, dialogs. A dimmer and a brighter step are derived from it. |
| `text` | All text. A muted variant is derived for labels, hints and secondary data. |
| `primary` | The one accent. Drawn as line: focus rings, active borders, progress, checkmarks. Filled in one place per view: the Start button, or a dialog's confirm button. |
| `success` | Done status badge and result log lines |
| `error` | Failed status badge and error log lines |

## How derivation works

From your 6 colours, the app computes:

- **Surface steps** - a dimmer variant (between surface and background) and a brighter variant
- **Muted text** - your text colour blended 60% over the background
- **Hairlines** - a faint line for dividers and a stronger one for control borders, both blended from text and background
- **Accent line** - your primary, made lighter (darker on a light background) until it is visible against every surface; hue and chroma are kept
- **Washes** - primary, success, error and amber at 12% over the background, used behind the selected row and the status badges
- **Status text** - success, error and amber, shifted in lightness until readable on their wash; hue and chroma are kept
- **Text on the accent** - black or white, whichever reads better on your primary (override with `text-on-primary`)

**Contrast is enforced.** Text colours are nudged toward white (toward black on a light background) until they reach 4.5:1 on every surface; lines until 3:1. Coloured tokens (accent line, success, error and amber text) change OKLCH lightness only, so a saturated primary stays saturated instead of fading to pastel; chroma drops only if the colour would leave the sRGB gamut. A theme with low-contrast text will look slightly brighter in the app than in its JSON. Colours you pass through as extra keys (`log-*`) are used exactly as written. Keys starting with `dx-` are ignored, so a theme cannot override the derived tokens and skip this check.

Light backgrounds work: the derivation flips direction when `background` is light.

## Optional overrides

Power users can override any derived value by adding extra keys to the `colors` object. The app derives a value only if the key is absent from your JSON.

```json
{
  "name": "Custom",
  "colors": {
    "background": "#161710",
    "surface": "#2B2B1F",
    "text": "#F8F8F2",
    "primary": "#F92672",
    "success": "#A6E22E",
    "error": "#F92672",
    "accent": "#A6E22E",
    "icon-cancelled": "#FD971F",
    "log-detect": "#AE81FF"
  }
}
```

### All overridable keys

| Key | Default derivation |
|-----|-------------------|
| `surface-dim` | Blend of surface toward background |
| `surface-bright` | Surface lightened (dark themes) or darkened (light themes) |
| `text-muted` | Text blended 60% over background |
| `accent` | Primary. Used for lines, focus and progress; the Start button fill stays `primary`. |
| `row-info` | Selected-row and hover wash: primary at 12% over background |
| `row-done` | Done badge wash: success at 12% over background |
| `row-failed` | Failed badge wash: error at 12% over background |
| `row-cancelled` | Cancelled badge wash: amber at 12% over background |
| `icon-done` | Done badge text and icon: success |
| `icon-failed` | Failed badge text and icon: error |
| `icon-cancelled` | Cancelled badge text and icon, and warning log lines: amber (#F08A1C) |
| `log-result` | Same as the done badge text |
| `log-warn` | Same as the cancelled badge text |
| `log-error` | Same as the failed badge text |
| `log-detect` | Muted text |
| `log-file` | Text |
| `log-text` | Text |
| `log-cmd` | Muted text |
| `text-on-primary` | Black or white, whichever reads better on primary. Used for text on the Start button and other primary fills; an override is used exactly as written, with no contrast check. |

Override values must be hex colours (`#rgb`, `#rrggbb`), except `log-*`, which also accept `rgb()` and `rgba()`.

`row-encoding`, `progress-fill` and `progress-bg` are no longer used: progress is drawn in the accent colour. Themes that set them still load.

## Backward compatibility

Themes using the old key names `base-100`, `base-300`, `base-content`, `secondary`, `info`, `status-done`, `status-failed` and `status-cancelled` still work. The app reads them as fallbacks when the new keys are absent. `base-200`, `neutral`, `warning` and `status-detect` are ignored.

## Tips

**Start from an existing theme.** Copy one of the built-in JSON files, rename it, and change the 6 values.

**Keep success and error bright.** The app mutes them automatically for badge backgrounds. Use the full-saturation version you'd want for an icon or text highlight.

**Test with a real queue.** Add a few files with different statuses (done, failed, pending) to see all the status badges together.

## Example themes

### Monokai

```json
{
  "name": "Monokai",
  "colors": {
    "background": "#161710",
    "surface": "#2B2B1F",
    "text": "#F8F8F2",
    "primary": "#F92672",
    "success": "#A6E22E",
    "error": "#F92672"
  }
}
```

### Solarised Dark

```json
{
  "name": "Solarised Dark",
  "colors": {
    "background": "#101014",
    "surface": "#24242B",
    "text": "#93A1A1",
    "primary": "#17B3A4",
    "success": "#00ea79",
    "error": "#ff525a"
  }
}
```

### Nord

```json
{
  "name": "Nord",
  "colors": {
    "background": "#1C222D",
    "surface": "#2E3747",
    "text": "#ECEFF4",
    "primary": "#76C3D8",
    "success": "#00ea79",
    "error": "#ff525a"
  }
}
```

### Creditor

```json
{
  "name": "Creditor",
  "colors": {
    "background": "#26070F",
    "surface": "#47101A",
    "text": "#F5E9CF",
    "primary": "#D9A21B",
    "success": "#4FD98A",
    "error": "#E5483F",
    "surface-bright": "#342E30",
    "icon-cancelled": "#E3B02A"
  }
}
```
