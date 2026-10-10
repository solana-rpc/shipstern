//! Configuration types for the Shipstern runtime.
use clap::Args;
use serde::Deserialize;
/// A helper trait for types that may or may not have a default value,
/// determined at runtime.
pub trait MaybeDefault: Sized {
    /// Get the default value for this type, if it exists.
    fn default_opt() -> Option<Self>;
}

impl<T: Default> MaybeDefault for T {
    #[inline]
    fn default_opt() -> Option<Self> { Some(Self::default()) }
}

/// Root configuration for [the Shipstern runtime](crate::Runtime).
#[derive(Debug, Args)]
pub struct ShipsternConfig<S>
where S: Args
{
    /// The source configuration.
    #[command(flatten)]
    pub source: S,

    /// The buffer configuration.
    #[command(flatten)]
    pub buffer: BufferConfig,
}

impl<'de, S> Deserialize<'de> for ShipsternConfig<S>
where S: Args + Deserialize<'de>
{
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where D: serde::Deserializer<'de> {
        #[derive(Deserialize)]
        #[serde(bound = "S: Deserialize<'de>")]
        struct Inner<S> {
            source: S,
            #[serde(default)]
            buffer: BufferConfig,
        }

        let Inner { source, buffer } = Inner::<S>::deserialize(deserializer)?;

        Ok(Self { source, buffer })
    }
}

const fn default_sources_channel_size() -> usize { 100 }

/// Job scheduler configuration.
#[derive(Debug, Clone, Copy, clap::Args, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct BufferConfig {
    /// The maximum number of concurrent jobs to run.  If unset, defaults to
    /// the number of CPUs.
    #[arg(long, env)]
    pub jobs: Option<usize>,
    /// The maximum number of concurrent sources to run.
    ///
    /// Defaults to 100: a `[buffer]` section omitting this key (including ones
    /// that predate the field) still deserializes.
    #[arg(long, env, default_value_t = 100)]
    #[serde(default = "default_sources_channel_size")]
    pub sources_channel_size: usize,
}

impl Default for BufferConfig {
    fn default() -> Self {
        Self {
            jobs: None,
            sources_channel_size: 100,
        }
    }
}

/// Helper type for blank configuration sections, such as the `Config` of a
/// [`FromConfig`](crate::sources::FromConfig) source that takes no settings.
#[derive(
    Default,
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    Hash,
    clap::Args,
    serde::Deserialize,
)]
pub struct NullConfig;

impl From<Option<NullConfig>> for NullConfig {
    #[inline]
    fn from(value: Option<NullConfig>) -> Self { value.unwrap_or_default() }
}

/// Helper type for optional configuration sections.
#[derive(Debug, serde::Deserialize)]
#[repr(transparent)]
pub struct OptConfig<T>(Option<T>);

impl<T> Default for OptConfig<T> {
    #[inline]
    fn default() -> Self { Self(None) }
}

impl<T> OptConfig<T> {
    /// Get the underlying `Option`.
    #[inline]
    pub fn opt(self) -> Option<T> { self.into() }
}

impl<T> From<T> for OptConfig<T> {
    #[inline]
    fn from(value: T) -> Self { Some(value).into() }
}

impl<T> From<Option<T>> for OptConfig<T> {
    #[inline]
    fn from(value: Option<T>) -> Self { Self(value) }
}

impl<T> From<OptConfig<T>> for Option<T> {
    #[inline]
    fn from(OptConfig(value): OptConfig<T>) -> Self { value }
}

impl<T> std::ops::Deref for OptConfig<T> {
    type Target = Option<T>;

    #[inline]
    fn deref(&self) -> &Self::Target { &self.0 }
}

impl<T> std::ops::DerefMut for OptConfig<T> {
    #[inline]
    fn deref_mut(&mut self) -> &mut Self::Target { &mut self.0 }
}

impl<T: clap::FromArgMatches> clap::FromArgMatches for OptConfig<T> {
    fn from_arg_matches(matches: &clap::ArgMatches) -> Result<Self, clap::Error> {
        T::from_arg_matches(matches).map(Into::into)
    }

    fn from_arg_matches_mut(matches: &mut clap::ArgMatches) -> Result<Self, clap::Error> {
        T::from_arg_matches(matches).map(Into::into)
    }

    fn update_from_arg_matches(&mut self, matches: &clap::ArgMatches) -> Result<(), clap::Error> {
        self.0
            .as_mut()
            .expect("Cannot update empty OptConfig")
            .update_from_arg_matches(matches)
    }

    fn update_from_arg_matches_mut(
        &mut self,
        matches: &mut clap::ArgMatches,
    ) -> Result<(), clap::Error> {
        self.0
            .as_mut()
            .expect("Cannot update empty OptConfig")
            .update_from_arg_matches_mut(matches)
    }
}

impl<T: clap::Args> clap::Args for OptConfig<T> {
    fn group_id() -> Option<clap::Id> { T::group_id() }

    fn augment_args(cmd: clap::Command) -> clap::Command { T::augment_args(cmd) }

    fn augment_args_for_update(cmd: clap::Command) -> clap::Command {
        T::augment_args_for_update(cmd)
    }
}

// Used for clap hacks below
#[allow(dead_code)] // Currently unused if feature 'prometheus' is disabled
fn update_clone<T: ToOwned, U: Into<T>, F: FnOnce(&mut U) -> V, V>(t: &mut T, f: F) -> V
where T::Owned: Into<U> {
    let mut u = t.to_owned().into();
    let ret = f(&mut u);
    *t = u.into();
    ret
}

#[cfg(test)]
mod tests {
    use super::BufferConfig;

    #[test]
    fn partial_buffer_fills_in_channel_size_default() {
        let cfg: BufferConfig =
            serde_json::from_str(r#"{ "jobs": 8 }"#).expect("partial config must deserialize");

        assert_eq!(cfg.sources_channel_size, 100);
    }

    #[test]
    fn full_buffer_is_respected() {
        let cfg: BufferConfig =
            serde_json::from_str(r#"{ "jobs": 4, "sources-channel-size": 250 }"#).unwrap();

        assert_eq!(cfg.sources_channel_size, 250);
    }

    /// The clap half of the same gap: without a `default_value_t`, a non-`Option`
    /// field makes `--sources-channel-size` a required argument.
    #[test]
    fn clap_defaults_channel_size_when_flag_is_absent() {
        use clap::Parser as _;

        #[derive(clap::Parser)]
        struct Opts {
            #[command(flatten)]
            buffer: BufferConfig,
        }

        let Opts { buffer } = Opts::try_parse_from(["test"]).expect("flag must be optional");

        assert_eq!(buffer.sources_channel_size, 100);
    }
}
