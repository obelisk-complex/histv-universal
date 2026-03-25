# Creating Themes

Honey, I Shrunk The Vids supports custom themes via simple JSON files. You pick 6 colours and the app derives everything else - borders, row tints, icon colours, progress bars - automatically.

## Where to put themes

Theme files live in a `themes/` folder next to the application executable. On first launch the app creates this folder and writes the built-in themes into it. Any `.json` file you drop in there will appear in the Theme dropdown.

## Theme file format

A theme file is a JSON object with a `name` and a `colors` object containing 6 colour values:

```json
{
  "name": "My Theme",
  "colors": {
    "background": "#1E1E1E",
    "surface": "#373737",
    "text": "#DCDCDC",
    "primary": "#0078D7",
    "success": "#4ade80",
    "error": "#f87171"
  }
}
```

That's all you need. Save it as a `.json` file in the `themes/` folder, restart the app, and it appears in the dropdown.

## What each colour controls

| Key | What it does |
|-----|-------------|
| `background` | The deepest layer - main app background, bottom bar |
| `surface` | Panels, inputs, table headers, buttons, borders. The app automatically creates lighter and darker variants from this. |
| `text` | All text. A muted variant at 55% opacity is derived automatically for secondary labels, hints, and status text. |
| `primary` | Buttons, active highlights, focused borders, selection outlines, progress bars, checkboxes, section headers |
| `success` | Done/completed status - used for the row tint (muted automatically), checkmark icon, and result log lines |
| `error` | Failed status - used for the row tint (muted automatically), X icon, and error log lines |

## How derivation works

From your 6 colours, the app computes:

- **Surface hierarchy** - a dimmer variant (between surface and background) and a brighter variant, giving depth without you picking three separate shades
- **Muted text** - your text colour at 55% opacity for labels and hints
- **Row tints** - your success and error colours overlaid at low opacity onto the background, producing muted tints that don't wash out the text
- **Encoding row** - a subtle brightening of the surface colour
- **Cancelled row** - a blend of the failed and encoding tints
- **Progress bar** - fill uses the done tint, unfilled uses the encoding tint
- **Icon colours** - success and error at full brightness for the small status icons
- **Log colours** - result lines use success, errors use error, warnings use an amber derived from the cancelled colour
- **Glass effects** - semi-transparent overlays adapted to whether your background is light or dark

## Optional overrides

Power users can override any derived value by adding extra keys to the `colors` object. The app derives a value only if the key is absent from your JSON.

```json
{
  "name": "Custom",
  "colors": {
    "background": "#272822",
    "surface": "#3E3D32",
    "text": "#F8F8F2",
    "primary": "#F92672",
    "success": "#A6E22E",
    "error": "#F92672",
    "accent": "#A6E22E",
    "progress-fill": "#3D1A2A",
    "progress-bg": "#3E3D32",
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
| `text-muted` | Text at 55% opacity |
| `accent` | Same as primary |
| `row-info` | Primary at 15% opacity over background |
| `row-done` | Success at 15% opacity over background |
| `row-encoding` | Surface shifted slightly brighter |
| `row-failed` | Error at 15% opacity over background |
| `row-cancelled` | Blend of row-failed and row-encoding |
| `progress-fill` | Same as row-done |
| `progress-bg` | Same as row-encoding |
| `icon-done` | Same as success |
| `icon-failed` | Same as error |
| `icon-cancelled` | Amber (#fb923c) |
| `log-result` | Same as icon-done |
| `log-warn` | Same as icon-cancelled |
| `log-error` | Same as icon-failed |
| `log-detect` | Purple (#a78bfa) |
| `log-file` | Light blue (#7dd3fc) |

## Backward compatibility

Themes using the old key names (`base-100`, `base-200`, `base-300`, `base-content`, `secondary`, `neutral`, `info`, `warning`, `status-done`, `status-failed`, `status-cancelled`, `status-detect`) still work. The app reads them as fallbacks when the new keys are absent.

## Tips

**Start from an existing theme.** Copy one of the built-in JSON files, rename it, and change the 6 values.

**Keep success and error bright.** The app mutes them automatically for row backgrounds. Use the full-saturation version you'd want for an icon or text highlight.

**Test with a real queue.** Add a few files with different statuses (done, failed, pending) to see all the row tints together.

## Example themes

### Monokai

```json
{
  "name": "Monokai",
  "colors": {
    "background": "#272822",
    "surface": "#3E3D32",
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
    "background": "#002B36",
    "surface": "#0A4050",
    "text": "#93A1A1",
    "primary": "#268BD2",
    "success": "#4ade80",
    "error": "#f87171"
  }
}
```

### Nord

```json
{
  "name": "Nord",
  "colors": {
    "background": "#2E3440",
    "surface": "#434C5E",
    "text": "#ECEFF4",
    "primary": "#88C0D0",
    "success": "#4ade80",
    "error": "#f87171"
  }
}
```