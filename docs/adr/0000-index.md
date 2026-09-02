# Architecture decision records

MADR-format records, numbered and immutable once accepted. A reversal is a new ADR superseding the
old one, never an edit to history. Each record names the evidence it rests on and what would
overturn it; a decision with no falsifier is a preference, not a decision.

| ADR | Title | Status |
|---|---|---|
| [0001](./0001-language-and-runtime.md) | Language and runtime | Accepted |
| [0002](./0002-http-layer.md) | HTTP layer: hyper rather than Pingora | Accepted |
| [0003](./0003-store-engine.md) | Store engine and object index | **Provisional - blocked on a foyer defect** |
| [0004](./0004-slice-size-and-keys.md) | Slice size, key scheme, generation semantics | Accepted |
| [0005](./0005-configuration-surface.md) | Configuration surface and lancache compatibility | Accepted |
| [0006](./0006-hosting-and-ci.md) | Repository hosting and CI topology | Accepted |
| [0007](./0007-access-log-compatibility.md) | Access-log compatibility with lancache tooling | Accepted |
| [0008](./0008-security-posture.md) | Security posture | Accepted |
