# Privacy Policy

_Last updated: 2026-07-14_

dowse is a local-first tool. This policy covers the dowse CLI, the dowse desktop app, and the dowse MCP server (including the Claude Desktop extension package).

## Data collection

dowse does not collect, transmit, or share any user data. There is no telemetry, no analytics, no crash reporting, and no account system.

## Usage and storage

- The full-text index is built from files you explicitly choose to index, and is stored **only on your device** (by default under your local application-data directory; the location can be overridden with the `DOWSE_INDEX_DIR` environment variable).
- The MCP server (`dowse mcp`) is **read-only**: it exposes search, preview, and index-status tools over stdio to the AI client you attach it to. It cannot modify your files or the index.
- Search results and file excerpts returned by MCP tools are delivered directly to the MCP client you connected (for example, Claude Desktop). What that client does with the data is governed by that client's own privacy policy.

## Network access

The dowse MCP server makes no network requests. All indexing, searching, OCR, and preview operations run entirely on your machine.

## Third-party sharing

None. dowse has no server-side component and shares no data with third parties.

## Data retention

The local index persists on your device until you delete it (via the dowse app, the CLI, or by removing the index directory). Uninstalling dowse does not upload anything; it simply removes local files.

## Contact

Questions or concerns: open an issue at <https://github.com/ltspace/dowse/issues>.
