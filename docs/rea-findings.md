# REA investigation findings

Working notes from REA (Reverse Engineer Anything) sessions. Purpose:
understand how the ad-supported incumbents implement browser capture and
what they phone home - so ripdl can do the useful part and none of the
creepy part. We record behavior and protocols here; no decompiled code or
proprietary assets are copied into ripdl.

## Target: Free Download Manager (browser extension)

- Status: pending first analysis
- Method: `npx -y rea-agents@latest analyze-javascript-application <ext-dir> --json`
- Questions:
  1. How does the extension detect a downloadable URL (listeners, filters)?
  2. What is the message shape between extension and the native app?
  3. What domains does the extension talk to besides the download target?

## Target: IDM (Internet Download Manager) extension

- Status: pending first analysis
- Same questions as above.

## Template for entries

- Target / version
- Method (REA commands used)
- Findings (protocol, endpoints, telemetry)
- What ripdl will adopt (protocol ideas) vs. reject (telemetry, nags)
