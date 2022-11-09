mod lamp;
mod td;
mod tester;

use std::borrow::Cow;

use clap::{Parser, ValueEnum};
use stable_eyre::eyre;
use tester::Tester;
use tracing::info;

#[derive(Debug, Parser)]
struct Cli {
    /// The remote host exposing Web of Things services.
    #[clap(long)]
    host: Option<String>,

    /// The remote port.
    #[clap(short, long)]
    port: Option<u16>,

    /// The Things to test.
    #[clap(value_enum)]
    tests: Vec<WotTest>,

    /// Also test Sifis hazards when possible.
    hazards: bool,
}

#[derive(Clone, Copy, Debug, ValueEnum)]
enum WotTest {
    Lamp,
    OnOffSwitch,
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> eyre::Result<()> {
    tracing_subscriber::fmt::init();
    stable_eyre::install()?;
    let Cli {
        host,
        port,
        tests,
        hazards,
    } = Cli::parse();

    let host = host.map(Cow::Owned).unwrap_or(Cow::Borrowed("localhost"));

    let tester = Tester::new(host, port);
    for test in tests {
        tester.test(test, hazards).await?;
    }

    info!("Everything seems fine");
    Ok(())
}
