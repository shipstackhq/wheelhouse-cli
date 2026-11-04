//! `wheelhouse-daemon pair | run | status | unpair`: the same commands as `wheelhouse daemon ...`.

#[tokio::main]
async fn main() {
    let code = wh_core::daemon_cli::main(std::env::args().skip(1).collect()).await;
    std::process::exit(code);
}
