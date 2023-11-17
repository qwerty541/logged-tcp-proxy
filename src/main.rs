use clap::Parser;
use logged_tcp_proxy::initialize_tcp_listener;
use logged_tcp_proxy::Arguments;
use std::convert::From;
use std::env;
use std::string::ToString;

#[tokio::main(flavor = "multi_thread", worker_threads = 4)]
async fn main() {
    let arguments = Arguments::parse();

    env::set_var("RUST_LOG", arguments.level.to_string());
    env_logger::builder()
        .parse_default_env()
        .format_target(false)
        .format_module_path(false)
        .format_timestamp(Some(From::from(arguments.precision)))
        .init();

    initialize_tcp_listener(arguments).await;
}
