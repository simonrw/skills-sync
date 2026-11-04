# Rust compatibility reference

This is the Rust implementation from `dda8338a` (Default to global prefix), retained as an executable oracle for the Go rewrite. Production builds use the Go package at the repository root.

Keep the reference implementation and its example manifest unchanged. Its integration tests accept `SKILLS_SYNC_BIN` to exercise the Go executable with the original assertions. See the root README for build and differential-test commands.
