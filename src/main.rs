mod args;
mod conn;
#[cfg(test)]
mod tests;

use args::Arguments;
use clap::Parser;
use conn::initialize_tcp_listener;

fn main() {
    let arguments = Arguments::parse();

    env_logger::builder()
        .parse_default_env()
        .filter_level(arguments.level.into())
        .format_target(false)
        .format_module_path(false)
        .format_timestamp(Some(From::from(arguments.precision)))
        .init();

    // Build the multi-threaded Tokio runtime by hand (instead of via the
    // `#[tokio::main]` macro) so its worker-thread count comes from the `--threads`
    // CLI argument at startup rather than being fixed at compile time. A runtime that
    // fails to build is logged and the process exits non-zero.
    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .worker_threads(arguments.threads as usize)
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(error) => {
            log::error!("Failed to build the Tokio runtime: {error}");
            std::process::exit(1);
        }
    };

    // A fatal startup failure (e.g. the listener address is unavailable) is logged
    // inside `initialize_tcp_listener`; exit non-zero so callers/scripts notice.
    if runtime
        .block_on(initialize_tcp_listener(arguments))
        .is_err()
    {
        std::process::exit(1);
    }
}
