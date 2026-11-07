use std::borrow::Cow;

use shipstern_core::{ParseResult, Parser, Prefilter, ProgramParser, Pubkey, SlotUpdate};

///
/// Passes slot status updates through. The default prefilter receives only the status at
/// the stream's commitment.
///
/// For `CreatedBank` and `Dead`, add `slot_interslot_updates(true)` with a `FilterPipeline`.
/// It also turns off the commitment filter, so the pipeline gets every status. A `Dead` status
/// has no `bank_id`. This works on the gRPC source; Fumarole ignores `interslot_updates`.
///
/// ```rust, ignore
/// Runtime::builder().slot(FilterPipeline::new(
///     SlotParser,
///     [Handler],
///     Prefilter::builder().slot_interslot_updates(true),
/// ))
/// ```
///
#[derive(Debug, Clone, Copy)]
pub struct SlotParser;

impl Parser for SlotParser {
    type Input = SlotUpdate;
    type Output = SlotUpdate;

    fn id(&self) -> Cow<'static, str> { "yellowstone::SlotParser".into() }

    fn prefilter(&self) -> Prefilter { Prefilter::builder().slots().build().unwrap() }

    async fn parse(&self, slot: &SlotUpdate) -> ParseResult<Self::Output> { Ok(slot.to_owned()) }
}

impl ProgramParser for SlotParser {
    #[inline]
    fn program_id(&self) -> Pubkey {
        "S111111111111111111111111111111111111111112"
            .parse()
            .unwrap()
    }
}

#[cfg(test)]
mod tests {
    use shipstern_core::SlotPrefilter;

    use super::*;

    /// A `FilterPipeline` merges its extra prefilter into the parser's, as here.
    #[test]
    fn extra_prefilter_turns_on_every_slot_status() {
        let mut prefilter = SlotParser.prefilter();
        prefilter.merge(
            Prefilter::builder()
                .slot_interslot_updates(true)
                .build()
                .unwrap(),
        );

        assert_eq!(
            prefilter.slot,
            Some(SlotPrefilter {
                filter_by_commitment: false,
                interslot_updates: Some(true),
            })
        );
    }
}
