## Tooling

- Use `go test ./...` for the Go implementation and `cargo nextest run --manifest-path tests/reference/rust/Cargo.toml` for the Rust compatibility reference.
- For compatibility changes, run the differential suite described in README.md. Keep the Rust reference implementation unchanged.
