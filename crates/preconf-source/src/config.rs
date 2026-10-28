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

/// Triton preconfs connection configuration.
#[derive(Clone, serde::Deserialize, clap::Args)]
#[serde(rename_all = "kebab-case")]
pub struct PreconfConfig {
    /// The Triton preconfs endpoint.
    #[arg(long, env, default_value = DEFAULT_ENDPOINT)]
    #[serde(default = "default_endpoint")]
    pub endpoint: String,
    /// The Triton token. The preconf feed must be enabled on it.
    // Without `hide_env_values`, `--help` prints the value of `X_TOKEN`.
    #[arg(long, env, hide_env_values = true)]
    pub x_token: Option<String>,
    /// Harmonic regions to stream, one stream each. A region named twice opens one stream.
    #[arg(long, env, value_delimiter = ',')]
    pub harmonic_regions: Vec<HarmonicRegion>,
    /// Seconds allowed for the TCP connect, again for the TLS handshake, and for each subscribe.
    #[arg(long, env, default_value_t = DEFAULT_TIMEOUT_SECS)]
    #[serde(default = "default_timeout")]
    pub timeout: u64,
}

// Written by hand so a `{config:?}` never prints the token.
impl std::fmt::Debug for PreconfConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PreconfConfig")
            .field("endpoint", &self.endpoint)
            .field("x_token", &self.x_token.as_ref().map(|_| "<redacted>"))
            .field("harmonic_regions", &self.harmonic_regions)
            .field("timeout", &self.timeout)
            .finish()
    }
}

fn default_endpoint() -> String { DEFAULT_ENDPOINT.to_string() }

fn default_timeout() -> u64 { DEFAULT_TIMEOUT_SECS }

#[cfg(test)]
mod tests {
    use clap::{Args, ValueEnum};
    use triton_preconfs_proto::preconfs;

    use super::{HarmonicRegion, PreconfConfig};

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

        check("HARMONIC_REGION", |r: HarmonicRegion| {
            preconfs::HarmonicRegion::from(r).as_str_name()
        });
    }
}
