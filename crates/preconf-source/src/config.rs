//! The `[source]` section of a preconf runtime.

use clap::ValueEnum;
use triton_preconfs_proto::preconfs;

const DEFAULT_ENDPOINT: &str = "https://preconfs.rpcpool.com";
const DEFAULT_TIMEOUT_SECS: u64 = 10;

/// A Harmonic region. Each region carries the slots of the leaders it serves, so a far region
/// adds slots, but they arrive late.
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

/// A BAM region. As for Harmonic, each region carries the slots of the leaders its nodes serve.
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
#[derive(Clone, serde::Deserialize, clap::Args)]
#[serde(default, rename_all = "kebab-case")]
pub struct PreconfConfig {
    /// The Triton preconfs endpoint.
    #[arg(long, env, default_value = DEFAULT_ENDPOINT)]
    pub endpoint: String,
    /// The Triton token. Each feed is enabled on it separately.
    // Without `hide_env_values`, `--help` prints the value of `X_TOKEN`.
    #[arg(long, env, hide_env_values = true)]
    pub x_token: Option<String>,
    /// Harmonic regions to stream, one stream each.
    #[arg(long, env, value_delimiter = ',')]
    pub harmonic_regions: Vec<HarmonicRegion>,
    /// BAM regions to stream, one stream each.
    #[arg(long, env, value_delimiter = ',')]
    pub bam_regions: Vec<BamRegion>,
    /// Seconds allowed for the TCP connect, again for the TLS handshake, and for each subscribe.
    #[arg(long, env, default_value_t = DEFAULT_TIMEOUT_SECS)]
    pub timeout: u64,
}

// clap does not read `Default`, so keep these equal to the `#[arg]` defaults.
impl Default for PreconfConfig {
    fn default() -> Self {
        Self {
            endpoint: DEFAULT_ENDPOINT.to_string(),
            x_token: None,
            harmonic_regions: Vec::new(),
            bam_regions: Vec::new(),
            timeout: DEFAULT_TIMEOUT_SECS,
        }
    }
}

// Written by hand so a `{config:?}` never prints the token.
impl std::fmt::Debug for PreconfConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PreconfConfig")
            .field("endpoint", &self.endpoint)
            .field("x_token", &self.x_token.as_ref().map(|_| "<redacted>"))
            .field("harmonic_regions", &self.harmonic_regions)
            .field("bam_regions", &self.bam_regions)
            .field("timeout", &self.timeout)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use clap::{Args, ValueEnum};
    use triton_preconfs_proto::preconfs;

    use super::{BamRegion, HarmonicRegion, PreconfConfig};

    /// The help text shows the env var name for the token, never its value.
    #[test]
    fn help_hides_the_token_value() {
        let command = PreconfConfig::augment_args(clap::Command::new("test"));
        let token = command
            .get_arguments()
            .find(|arg| arg.get_id() == "x_token")
            .expect("the config has an x_token argument");

        assert!(token.is_hide_env_values_set());
    }

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

    /// A BAM-only token sets only `bam-regions`, so `harmonic-regions` must stay optional.
    #[test]
    fn a_table_with_only_bam_regions_parses() {
        let config: PreconfConfig = serde_json::from_str(r#"{"bam-regions": ["fra"]}"#).unwrap();

        assert!(config.harmonic_regions.is_empty());
        assert_eq!(config.bam_regions, [BamRegion::Fra]);
    }
}
