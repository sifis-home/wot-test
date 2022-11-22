mod lamp;
mod tester;

use std::borrow::Cow;

use clap::{Parser, ValueEnum};
use stable_eyre::eyre;
use tracing::info;

use crate::tester::Tester;

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
}

#[derive(Clone, Copy, Debug, ValueEnum)]
enum WotTest {
    Lamp,
    OnOffSwitch,
    LightEmitter,
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> eyre::Result<()> {
    tracing_subscriber::fmt::init();
    stable_eyre::install()?;
    let Cli { host, port, tests } = Cli::parse();

    let host = host.map(Cow::Owned).unwrap_or(Cow::Borrowed("localhost"));

    let tester = Tester::new(host, port);
    for test in tests {
        tester.test(test).await?;
    }

    info!("Everything seems fine");
    Ok(())
}
