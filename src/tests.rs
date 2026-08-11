//! In-crate integration tests for the TCP proxy relay.
//!
//! These run in-process via `cargo test` (no `lib` target, nothing exposed
//! publicly or on docs.rs) and behave identically in CI and locally. Each test
//! brings its own pure-Tokio echo server, so there are no external dependencies
//! (python, netcat, ...) and no network access beyond `127.0.0.1`. All listeners
//! bind to ephemeral ports (`127.0.0.1:0`) and all I/O is bounded by a timeout,
//! so the tests are deterministic and never collide across parallel jobs.
//!
//! The suite is split across submodules by the behavior they cover; `helpers`
//! and `log_capture` hold the scaffolding they share. The whole tree is compiled
//! only under `#[cfg(test)]`, via the gated `mod tests;` declaration in
//! [`main.rs`](main.rs), so the submodules need no `cfg` attribute of their own.

mod accept_loop;
mod cli_args;
mod errors;
mod helpers;
mod hostname;
mod idle_timeout;
mod log_capture;
mod real_protocols;
mod relay;
mod teardown;
