//! Helpers for working with parsers whose output can be converted to an
//! equivalent Protobuf representation.

use shipstern_proto::prost::{Message, Name};

use super::{Parser, ProgramParser};

/// Defines a `pub(crate)` `IntoProto` trait for converting types from other
/// crates into Protobuf messages:
/// ```
/// # use shipstern_core::shipstern_proto::prost::Message;
/// pub(crate) trait IntoProto<T: Message + Send + Sync> {
///     fn into_proto(self) -> T;
/// }
/// ```
#[macro_export]
macro_rules! proto_helper_traits {
    () => {
        pub(crate) trait IntoProto<T: $crate::shipstern_proto::prost::Message + Send + Sync> {
            fn into_proto(self) -> T;
        }
    };
}

/// A parser whose output can be converted to an equivalent Protobuf message
pub trait ParseProto: Parser {
    /// The Protobuf message type corresponding to this parser's output
    type Message: Message + Name + Send + Sync;

    /// Convert a value output by this parser into its Protobuf message
    fn output_into_message(value: Self::Output) -> Self::Message;
}

/// Wrapper for converting the output of a parser into Protobuf messages
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(transparent)]
pub struct Proto<T>(T);

impl<T> Proto<T> {
    /// Wrap the provided parser in a new `Proto` instance
    #[inline]
    pub fn new(value: T) -> Self { Self(value) }

    /// Return the parser contained within this `Proto` instance
    #[inline]
    pub fn into_inner(self) -> T { self.0 }
}

impl<T> From<T> for Proto<T> {
    #[inline]
    fn from(value: T) -> Self { Self(value) }
}

impl<T> std::ops::Deref for Proto<T> {
    type Target = T;

    #[inline]
    fn deref(&self) -> &Self::Target { &self.0 }
}

impl<T> std::ops::DerefMut for Proto<T> {
    #[inline]
    fn deref_mut(&mut self) -> &mut Self::Target { &mut self.0 }
}

impl<T: ParseProto + ProgramParser + Sync> ProgramParser for Proto<T>
where T::Input: Sync
{
    #[inline]
    fn program_id(&self) -> crate::Pubkey { self.0.program_id() }
}

impl<T: ParseProto + Sync> Parser for Proto<T>
where T::Input: Sync
{
    type Input = T::Input;
    type Output = T::Message;

    #[inline]
    fn id(&self) -> std::borrow::Cow<'static, str> { self.0.id() }

    #[inline]
    fn prefilter(&self) -> crate::Prefilter { self.0.prefilter() }

    #[inline]
    async fn parse(&self, value: &Self::Input) -> crate::ParseResult<Self::Output> {
        self.0.parse(value).await.map(T::output_into_message)
    }
}
