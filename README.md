# Rust macros & FFI experiments

A small collection of procedural macros and interop code I've written to dig
into parts of Rust I use in production but wanted to understand end to end.
Everything here is original code written for this repo — rewritten from
scratch as small, self-contained examples rather than pulled from any
proprietary codebase.

## Crates

- [`proc-pbac-macro`](proc-pbac-macro) — an attribute macro,
  `#[require_policies(policies = "...")]`, that injects a policy-based
  access-control check into an async method at compile time.
- [`geo-bindgen-macro`](geo-bindgen-macro) — a derive macro,
  `#[derive(GeoFfiType)]`, that generates the `From`/`Into` glue between an
  idiomatic Rust struct and a native geometry type on the other side of an
  FFI boundary (field-by-field, with optional per-field renames), so that
  boilerplate doesn't have to be hand-written and re-written every time the
  native layout changes.
- [`geo-distance-ffi`](geo-distance-ffi) — a Rust/C++ interop built with
  [`cxx`](https://cxx.rs): haversine distance and a nearest-match scan
  (matching a set of "requesters" to the closest "candidates" by
  coordinates) implemented in C++ and called from Rust through a typed
  bridge. Meant to illustrate why you'd reach for a C++ core for a
  compute-heavy inner loop instead of paying per-call FFI overhead. Its
  `GeoPoint` type (`src/point.rs`) is generated with `geo-bindgen-macro`
  above.
