# Native Gorak language server

Clean, readable, correct code is the primary standard. Optimize measured workloads without obscure machinery or unsafe code. Use explicit types for identities and source locations; explain invariants, not obvious statements. Keep source handling, syntax, analysis, workspace lifecycle and LSP transport separate.

Run `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings` and `cargo test` after changes. Release benchmarks must compare equivalent operations and disclose correctness gaps. Never treat missing analysis as a speed improvement.

Use independently authored synthetic fixtures. Private application corpora are read-only acceptance inputs; never copy their text, names, connection identities, paths, caches or runtime logs into this repository. Export only anonymized aggregate metrics. Preserve original UTF-16 locations through WML and preprocessing. Keep direct application include order, image boundaries, ambiguity and dependency invalidation explicit.

Rust is the shipping language server. Retain the synthetic differential fixtures as historical regression coverage. Rename must refuse ambiguous or unsupported coverage and return versioned edits. Do not automatically connect to OpenROAD, sync, push, compile, execute source, install globally or publish.
