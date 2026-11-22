# ripdl UI design source

This is the design reference used for the native spike rework, not a production
frontend.

## Tool

[UICanvas](https://github.com/markvely/uicanvas) 1.3.0 (MIT), a local,
self-hosted infinite-canvas design tool. It was run with Node only; no cloud
service, account, or model was used. The artboards were created through its MCP
canvas tools and rendered locally in Edge.

The design follows UICanvas's design-spec-first workflow:

1. `init_project` with ripdl color and font tokens
2. `finalize_design_spec`
3. page artboards for Main, Options (Connection), Options (LAN), Add URL, and
   Details

## Design system

- Segoe UI / Segoe UI Semibold
- Fluent-style icon set
- Light surfaces, 1 px borders, 8 px radius
- Teal accent `#0F766E`
- 8 px spacing rhythm
- Green completed, blue active, amber warning, red destructive

The app target uses Segoe UI plus Segoe MDL2 Assets for icons so the native
spike and the design reference share the same typography and icon language.

## Files

- `base.css` - tokens, shell, toolbar, table, options, details, empty state
- `icons.html` - inline SVG symbol set
- `main.html` - main window and task table
- `options-connection.html` - Options / Connection
- `options-lan.html` - Options / LAN
- `add.html` - Add URL with Advanced expanded
- `details.html` - task details and file/part progress

Exported design shots live in `../qa/design/`:

- `main.png`
- `options-connection.png`
- `options-lan.png`
- `add.png`
- `details.png`

## Constraint

The production spike is egui, so these are layout, type, spacing, and color
references. They are not shipped as HTML at runtime.
