//! End-to-end peer driver for the NixOS VM test network.

mod net;
mod run;

#[tokio::main(flavor = "multi_thread")]
async fn main() {
    let cli = match run::parse_args(std::env::args().collect()) {
        Ok(cli) => cli,
        Err(e) => {
            eprintln!("fungi-e2e: {e}");
            std::process::exit(2);
        }
    };
    if let Err(e) = run::run(cli).await {
        eprintln!("fungi-e2e: {e}");
        std::process::exit(1);
    }
}
