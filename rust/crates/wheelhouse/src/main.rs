//! `wheelhouse`. So far only `wheelhouse daemon <pair|run|status|unpair>` is here (WH-3); the board commands, login, doctor and the rest
//! of the CLI are WH-7 and still run from the Node CLI.

#[tokio::main]
async fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let code = match args.first().map(String::as_str) {
        Some("daemon") => wh_core::daemon_cli::main(args[1..].to_vec()).await,
        Some("version") | Some("--version") => {
            println!("{}", wh_core::daemon::VERSION);
            0
        }
        _ => {
            eprintln!("This build only has the daemon commands so far: wheelhouse daemon pair|run|status|unpair");
            2
        }
    };
    std::process::exit(code);
}
