mod args;
mod conn;
#[cfg(test)]
mod tests;

use args::Arguments;
use clap::Parser;
use conn::initialize_tcp_listener;

#[tokio::main(flavor = "multi_thread", worker_threads = 4)]
async fn main() {
    let arguments = Arguments::parse();

    env_logger::builder()
        .parse_default_env()
        .filter_level(arguments.level.into())
        .format_target(false)
        .format_module_path(false)
        .format_timestamp(Some(From::from(arguments.precision)))
        .init();

    // A fatal startup failure (e.g. the listener address is unavailable) is logged
    // inside `initialize_tcp_listener`; exit non-zero so callers/scripts notice.
    if initialize_tcp_listener(arguments).await.is_err() {
        std::process::exit(1);
    }
}
