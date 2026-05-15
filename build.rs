//! Build script for Pietro (M7).
//!
//! Job: make sure `frontend/dist/` is in good shape before `rust-embed`
//! freezes it into the binary. Two modes, on purpose:
//!
//!   * **Release builds** (`PROFILE=release`): if `frontend/dist/index.html`
//!     is missing, fail the build with a friendly hint. A release binary
//!     without the SPA is a half-state — `pietro.md` §19 / §13.
//!
//!   * **Debug builds**: emit a `cargo:warning=` and let the build proceed.
//!     The intended dev loop is Vite on a separate port (see §13 "Dev loop"),
//!     and the runtime SPA handler in `src/spa.rs` serves a clear notice
//!     page rather than stale bytes. Failing here would block all backend
//!     work whenever someone wipes `frontend/dist/`.
//!
//! Re-run only when the embed source actually changes — re-running on every
//! build would force a recompile each `npm run build`.

use std::path::Path;

fn main() {
    let dist = Path::new("frontend/dist");
    let index = dist.join("index.html");

    // Re-run if the embed root or its key file changes.
    println!("cargo:rerun-if-changed=frontend/dist/index.html");
    println!("cargo:rerun-if-changed=frontend/dist/assets");

    if index.exists() {
        return;
    }

    let profile = std::env::var("PROFILE").unwrap_or_default();
    let hint = "the React bundle is missing. \
        Build it with `cd frontend && npm install && npm run build`, \
        then rebuild Pietro. (See pietro.md §13.)";

    if profile == "release" {
        // Hard fail — a release build with an empty embed is a half-state.
        // Use `cargo:warning=` first so the message survives any compiler
        // truncation, then panic to break the build.
        println!("cargo:warning=Pietro release build: {hint}");
        panic!("frontend/dist/index.html is missing: {hint}");
    } else {
        println!(
            "cargo:warning=Pietro debug build: frontend/dist/index.html is \
             missing — the binary will serve a dev-mode notice page instead \
             of the SPA. {hint}"
        );
    }
}
