# S05 - Link grabber / bundles

Status: `spec ready`. Read `SHARED.md`, `S02-add-url.md`.

## Goal

Paste a page or link list -> review -> one package/folder. This is ripdl's
differentiator against IDM's stronger grabber workflow.

## IDM behavior

- Paste URL -> recursive page crawl with filters.
- Review list of discovered files.
- Select subset -> download as package with one folder.

## ripdl target

- Paste links or a page URL.
- Preview step: parsed links with filename, size (when known), weak-source
  badge, include/exclude checkboxes.
- Duplicate URLs dropped with a count shown.
- "Start" creates one Bundle task via `AddBundle`.
- Bundle row shows "3/7 parts" and aggregate progress.

## State model

```text
source_text: String
links: Vec<BundleLink>          // url, filename, weak
selected: Set<usize>
group_name: String
parse_state: idle | parsing | ready | error
duplicate_count: usize
```

## Interactions

- Paste -> parse -> review inline; no modal storm.
- Toggle weak candidates; recognized files checked by default.
- Start -> one RPC call; dialog closes on `Added`.

## Acceptance

- [ ] 100-link cap enforced with visible message.
- [ ] Packages land in one task-owned folder (existing bundle engine).
- [ ] Duplicate count shown before start.
- [ ] Screenshots: empty, parsing, ready, error.

## Work IDs

FEATURES.csv: IDM-019, IDM-020, RIP-041.
Roadmap: GRB-001/002, BUN-*.

## Do not

- Do not auto-download on paste; explicit Start is required.
