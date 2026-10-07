# gorak language server

OpenROAD navigation, references, outline, hover and completion for exported [gorak](https://github.com/dougwhite/gorak) source repositories. Runs locally; no database connection or telemetry.

For VS Code, install [gorak OpenROAD](https://github.com/dougwhite/gorak-vscode-ext). Standalone Windows x64 and Linux x64 binaries are on [Releases](https://github.com/dougwhite/gorak-lsp-rs/releases); launch with `--stdio`.

Alpha: analysis is incomplete and is not an OpenROAD compiler. Interactive requests take priority over workspace searches. Cancellation is cooperative; reading or parsing a single large file can still delay responses. Caches may contain source and stay in the local user profile. No source is uploaded.

Build with Rust 1.88+: `cargo test --locked`, then `python scripts/release.py`. Tag `v<version>` to test and publish an alpha release through GitHub Actions. Release packages include dependency notices; public API catalogue provenance is in [catalogue/PROVENANCE.md](catalogue/PROVENANCE.md).

MIT licensed.

Frame analysis uses explicit WML values, including typed column prototypes and native character processing instructions. Native stylesheet JSON is creation-palette data, not application declarations or a source of missing field properties.

Contract 12 member metadata accepts declaration strings or structured objects with
a `declaration` property, including native `PRIVATE`, array and default clauses.
Remarks and tagged values remain opaque. Metadata navigation uses original TOML
spans; escaped identifiers are diagnosed and cannot be renamed, and escaped type
spellings have no guessed reference location. Available project `core` source
precedes other direct includes; explicit image dependencies still block guessed
source resolution.
