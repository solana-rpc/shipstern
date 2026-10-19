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

/// A BAM region. The same rule as for Harmonic: nearby regions only.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, serde::Deserialize, ValueEnum)]
#[serde(rename_all = "kebab-case")]
pub enum BamRegion {
    Ams,
    Dfw,
    Dub,
    Ewr,
    Fra,
    Hkg,
    Iad,
    Lax,
    Lon,
    Pit,
    Sea,
    Sin,
    Slc,
    Sqq,
    Tyo,
}

impl From<BamRegion> for preconfs::BamRegion {
    fn from(region: BamRegion) -> Self {
        match region {
            BamRegion::Ams => Self::Ams,
            BamRegion::Dfw => Self::Dfw,
            BamRegion::Dub => Self::Dub,
            BamRegion::Ewr => Self::Ewr,
            BamRegion::Fra => Self::Fra,
            BamRegion::Hkg => Self::Hkg,
            BamRegion::Iad => Self::Iad,
            BamRegion::Lax => Self::Lax,
            BamRegion::Lon => Self::Lon,
            BamRegion::Pit => Self::Pit,
            BamRegion::Sea => Self::Sea,
            BamRegion::Sin => Self::Sin,
            BamRegion::Slc => Self::Slc,
            BamRegion::Sqq => Self::Sqq,
            BamRegion::Tyo => Self::Tyo,
        }
    }
}

/// Triton preconfs connection configuration. At least one region of either feed is needed. A
/// region named twice in one list opens one stream; the same region in both lists opens one
/// stream per feed.
#[derive(Debug, serde::Deserialize, clap::Args)]
#[serde(rename_all = "kebab-case")]
pub struct PreconfConfig {
    /// The Triton preconfs endpoint.
    #[arg(long, default_value = DEFAULT_ENDPOINT)]
    #[serde(default = "default_endpoint")]
    pub endpoint: String,
    /// The Triton token. Each feed is enabled on it separately.
    #[arg(long)]
    pub x_token: Option<String>,
    /// Harmonic regions to stream, one stream each.
    #[arg(long, value_delimiter = ',')]
    #[serde(default)]
    pub harmonic_regions: Vec<HarmonicRegion>,
    /// BAM regions to stream, one stream each.
    #[arg(long, value_delimiter = ',')]
    #[serde(default)]
    pub bam_regions: Vec<BamRegion>,
    /// Seconds allowed for the TCP connect, and again for the TLS handshake.
    #[arg(long, default_value_t = DEFAULT_TIMEOUT_SECS)]
    #[serde(default = "default_timeout")]
    pub timeout: u64,
}

fn default_endpoint() -> String { DEFAULT_ENDPOINT.to_string() }

fn default_timeout() -> u64 { DEFAULT_TIMEOUT_SECS }

#[cfg(test)]
mod tests {
    use clap::ValueEnum;
    use triton_preconfs_proto::preconfs;

    use super::{BamRegion, HarmonicRegion};

    /// Each config name must reach the proto value of the same name, or a stream opens on
    /// the wrong region.
    #[test]
    fn region_names_match_the_proto() {
        fn check<T: ValueEnum + Copy>(prefix: &str, proto_name: fn(T) -> &'static str) {
            for region in T::value_variants() {
                let name = region
                    .to_possible_value()
                    .unwrap()
                    .get_name()
                    .to_uppercase();
                assert_eq!(proto_name(*region), format!("{prefix}_{name}"));
            }
        }

        check("BAM_REGION", |r: BamRegion| {
            preconfs::BamRegion::from(r).as_str_name()
        });
        check("HARMONIC_REGION", |r: HarmonicRegion| {
            preconfs::HarmonicRegion::from(r).as_str_name()
        });
    }
}
