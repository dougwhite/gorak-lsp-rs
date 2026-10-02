# Synthetic behavioural oracle

`oracle.json` contains deduplicated query inputs and outputs captured from the existing TypeScript server's independently authored unit tests. Inputs are restricted to synthetic `file:///repo/`, `file:///workspace/`, `file:///project/` and `file:///synthetic/` workspaces. No application corpus source, connection settings or environment identities are included.

The differential test preserves definition and outline order. References, completion candidates and separate-document rename edits are compared without ordering because their protocol meaning is unchanged. Refusal cases assert refusal rather than matching English error wording.

One documented representation difference is accepted: native call-expression diagnostic spans include the closing parenthesis. The test requires the same diagnostic count, message, severity, code, start position and all other fields, with precisely one extra end character for these captured calls. It cannot hide missing analysis or extra warnings.

These cases test baseline behaviour; they do not prove full OpenROAD compiler compatibility. Independent Rust regression tests cover lifecycle, malformed input, caching and deliberate corrections beyond the baseline.
