#[derive(clap::Parser, Debug, Clone)]
#[clap(version = env!("CARGO_PKG_VERSION"))]
pub struct Opts {
    // #[clap(short, long, help = "The address of serve")]
    // pub listen: SocketAddr,
    #[clap(short, long, help = "Generate a nostr keypair")]
    pub generate: bool,
    #[clap(
        short,
        long,
        parse(from_occurrences),
        help = "Loglevel: -v(Info), -vv(Debug), -vvv+(Trace)"
    )]
    pub verbose: u8,
    #[clap(short, long, help = "The path of Config", default_value = "bc.toml")]
    pub config: String,
}

use tracing::Level;
impl Opts {
    pub fn log(&self) -> Level {
        match self.verbose {
            0 => Level::WARN,
            1 => Level::INFO,
            2 => Level::DEBUG,
            _ => Level::TRACE,
        }
    }
    pub fn parse_config(&self) -> anyhow::Result<Config> {
        let fc = std::fs::read_to_string(&self.config)?;
        let c = toml::from_str::<Config>(&fc)?;

        for u in &c.relays {
            url::Url::parse(u)?;
        }

        Ok(c)
    }
}

use std::net::SocketAddr;
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Config {
    pub listen: SocketAddr,
    pub database: String,
    pub timeout_ms: u64,
    pub relays: Vec<String>,
    pub cashu: ConfigCashu,
}

use keychat_rust_ffi_plugin::api_cashu::cashu_wallet::Url;
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ConfigCashu {
    pub mints: Vec<Url>,
    pub database: String,
    #[serde(default)]
    pub allow_pending: bool,
    #[serde(default)]
    pub allow_free: bool,
}
