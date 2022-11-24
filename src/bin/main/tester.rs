mod lamp;
mod light_emitter;
mod on_off_switch;

use std::borrow::Cow;

use reqwest::Client;
use stable_eyre::eyre;
use wot_test::tester::Requester;

use crate::WotTest;

pub(crate) struct Tester {
    host: Cow<'static, str>,
    port: Option<u16>,
    client: Client,
}

impl Tester {
    pub(crate) fn new(host: Cow<'static, str>, port: Option<u16>) -> Self {
        let client = Client::new();

        Self { host, port, client }
    }

    pub(crate) async fn test(&self, test: WotTest) -> eyre::Result<()> {
        match test {
            WotTest::Lamp => lamp::run_test(self).await,
            WotTest::OnOffSwitch => on_off_switch::run_test(self).await,
            WotTest::LightEmitter => light_emitter::run_test(self).await,
        }
    }
}

impl Requester for Tester {
    #[inline]
    fn host(&self) -> &str {
        &self.host
    }

    #[inline]
    fn port(&self) -> Option<u16> {
        self.port
    }

    #[inline]
    fn client(&self) -> &Client {
        &self.client
    }
}
