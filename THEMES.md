# Creating Themes

Honey, I Shrunk The Vids supports custom themes via simple JSON files. The app ships with Default Dark and Default Light, but you can create your own to match whatever aesthetic you're after.

## Where to put themes

Theme files live in a `themes/` folder next to the application executable. On first launch the app creates this folder and writes the two default themes into it. Any `.json` file you drop in there will appear in the Theme dropdown on the Encoding Settings tab.

## Theme file format

A theme file is a JSON object with two fields: a `name` (what shows up in the dropdown) and a `colors` object mapping colour variable names to hex values.

Here's a minimal example:

```json
{
  "name": "Monokai",
  "colors": {
    "primary": "#F92672",
    "secondary": "#75715E",
    "accent": "#A6E22E",
    "neutral": "#3E3D32",
    "base-100": "#272822",
    "base-200": "#2D2E27",
    "base-300": "#3E3D32",
    "base-content": "#F8F8F2",
    "info": "#1B3A4B",
    "success": "#1B4332",
    "warning": "#3D3400",
    "error": "#4A1520",
    "status-done": "#4ade80",
    "status-failed": "#f87171",
    "status-cancelled": "#fb923c",
    "status-detect": "#a78bfa"
  }
}
```

Save it as something like `monokai.json` in the `themes/` folder, restart the app, and it'll appear in the dropdown.

## Colour reference

Every colour in the UI is driven by these 16 variables. Here's what each one controls:

### Core colours

| Variable | Used for |
|----------|----------|
| `primary` | Buttons, active tab highlights, progress bar, selected items, focused input borders, the splitter on hover, drop zone borders, checkboxes |
| `secondary` | Inactive tab text, status text, log toggle text, status bar text |
| `accent` | Selection outline around queue rows (often the same as `primary`, but you can make it different) |

### Background layers

These three form a layered hierarchy - `base-100` is the deepest background, `base-200` is panels and tables, and `base-300` is elevated surfaces and borders.

| Variable | Used for |
|----------|----------|
| `base-100` | Main app background, bottom bar background |
| `base-200` | Queue table background, modal backgrounds, context menu background |
| `base-300` | Table headers, splitter, tab bar border, input field backgrounds, buttons, log toggle bar, borders throughout |
| `base-content` | All primary text - filenames, labels, headings, button text, table content |
| `neutral` | Input borders, table header bottom border, button hover background, splitter default colour |

### Status row tints

These colour the background of queue rows based on their encoding status. They should be quite muted - the text colour is always `base-content`, so these need enough contrast to remain readable.

| Variable | Used for |
|----------|----------|
| `info` | Probing status row tint |
| `success` | Done / completed row tint |
| `warning` | Encoding in progress row tint |
| `error` | Failed row tint |

### Status icon colours

These colour the small status icons in the queue's Status column and in log lines. They should be bright enough to read at icon size. If omitted, sensible defaults are used.

| Variable | Used for |
|----------|----------|
| `status-done` | Checkmark icon for completed files, "result" log lines |
| `status-failed` | X icon for failed files |
| `status-cancelled` | Circle-slash icon for cancelled files |
| `status-detect` | Encoder detection log lines |

## Tips for good themes

**Start from an existing theme.** Copy `default-dark.json` or `default-light.json`, rename it, and tweak the values. This way you know every variable is present and the structure is valid.

**Keep status tints subtle.** The status colours (`info`, `success`, `warning`, `error`) are used as full row backgrounds with white/light text on top. If you make them too bright, the text becomes unreadable. For dark themes, keep them dark and saturated. For light themes, use pale pastels.

**Test the `base` hierarchy.** The three `base-` colours need to create a clear visual hierarchy: `base-100` darkest (or lightest), `base-300` most prominent. If `base-200` and `base-300` are too similar, the UI looks flat.

**Mind the contrast.** `base-content` is your text colour - make sure it's readable against `base-100`, `base-200`, and `base-300`. Similarly, `primary` needs to be visible against `base-300` (since buttons use `base-300` as their default background).

**`primary` and `accent` can differ.** If you want the selection highlight on queue rows to be a different colour from buttons and tabs, set `accent` to something distinct from `primary`.

## Example themes

### Solarized Dark

```json
{
  "name": "Solarised Dark",
  "colors": {
    "primary": "#268BD2",
    "secondary": "#586E75",
    "accent": "#2AA198",
    "neutral": "#073642",
    "base-100": "#002B36",
    "base-200": "#073642",
    "base-300": "#0A4050",
    "base-content": "#93A1A1",
    "info": "#0D3D56",
    "success": "#0D3D2A",
    "warning": "#3D3A0D",
    "error": "#4A1A1A",
    "status-done": "#4ade80",
    "status-failed": "#f87171",
    "status-cancelled": "#fb923c",
    "status-detect": "#a78bfa"
  }
}
```

### Nord

```json
{
  "name": "Nord",
  "colors": {
    "primary": "#88C0D0",
    "secondary": "#616E88",
    "accent": "#81A1C1",
    "neutral": "#3B4252",
    "base-100": "#2E3440",
    "base-200": "#3B4252",
    "base-300": "#434C5E",
    "base-content": "#ECEFF4",
    "info": "#2E3D50",
    "success": "#2E4038",
    "warning": "#4A4530",
    "error": "#4A2E2E",
    "status-done": "#4ade80",
    "status-failed": "#f87171",
    "status-cancelled": "#fb923c",
    "status-detect": "#a78bfa"
  }
}
```

### Vempire

```json
{
  "name": "Vempire",
  "colors": {
    "primary": "#BD93F9",
    "secondary": "#6272A4",
    "accent": "#FF79C6",
    "neutral": "#44475A",
    "base-100": "#282A36",
    "base-200": "#2D2F3D",
    "base-300": "#44475A",
    "base-content": "#F8F8F2",
    "info": "#1A2744",
    "success": "#1A3A2A",
    "warning": "#3D3A1A",
    "error": "#4A1A2A",
    "status-done": "#4ade80",
    "status-failed": "#f87171",
    "status-cancelled": "#fb923c",
    "status-detect": "#a78bfa"
  }
}
```