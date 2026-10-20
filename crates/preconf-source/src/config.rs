//! The `[source]` section of a preconf runtime.

use clap::ValueEnum;
use triton_preconfs_proto::preconfs;

const DEFAULT_ENDPOINT: &str = "https://preconfs.rpcpool.com";
const DEFAULT_TIMEOUT_SECS: u64 = 10;

/// A Harmonic region. Regional streams carry different slots, so subscribe to nearby
/// regions only: a far region adds latency, not coverage.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, serde::Deserialize, ValueEnum)]
#[serde(rename_all = "kebab-case")]
pub enum HarmonicRegion {
    Ams,
    Ewr,
    Fra,
    Lon,
    Tyo,
    Sgp,
    Slc,
}

impl From<HarmonicRegion> for preconfs::HarmonicRegion {
    fn from(region: HarmonicRegion) -> Self {
        match region {
            HarmonicRegion::Ams => Self::Ams,
            HarmonicRegion::Ewr => Self::Ewr,
            HarmonicRegion::Fra => Self::Fra,
            HarmonicRegion::Lon => Self::Lon,
            HarmonicRegion::Tyo => Self::Tyo,
            HarmonicRegion::Sgp => Self::Sgp,
            HarmonicRegion::Slc => Self::Slc,
        }
    }
}

/// Triton preconfs connection configuration.
#[derive(Debug, serde::Deserialize, clap::Args)]
#[serde(rename_all = "kebab-case")]
pub struct PreconfConfig {
    /// The Triton preconfs endpoint.
    #[arg(long, default_value = DEFAULT_ENDPOINT)]
    #[serde(default = "default_endpoint")]
    pub endpoint: String,
    /// The Triton token. The preconf feed must be enabled on it.
    #[arg(long)]
    pub x_token: Option<String>,
    /// Harmonic regions to stream, one stream each. A region named twice opens one stream.
    #[arg(long, value_delimiter = ',')]
    pub harmonic_regions: Vec<HarmonicRegion>,
    /// Seconds allowed for the TCP connect, and again for the TLS handshake.
    #[arg(long, default_value_t = DEFAULT_TIMEOUT_SECS)]
    #[serde(default = "default_timeout")]
    pub timeout: u64,
}

fn default_endpoint() -> String { DEFAULT_ENDPOINT.to_string() }

fn default_timeout() -> u64 { DEFAULT_TIMEOUT_SECS }
