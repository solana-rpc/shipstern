use shipstern_core::{instruction::InstructionUpdate, Parser};
use shipstern_mock::{load_fixture, FixtureData};
use shipstern_proc_macro::include_shipstern_parser;
use shipstern_test_utils::p;

include_shipstern_parser!("../idls/pump_fun.json");

const BUY_TX: &str =
    "5MYEyDocQHfrEAHf42Cro1Y59vrSDsdpXQNsNG41ZMdibBmw4AAHzHTa2SFWTkYSQiqjmTBRyD1Z61rkhEgRBtAi";

/// pump.fun emits each `TradeEvent` as a self-CPI and as a `Program data:` log
/// line, so a buy must still yield one event.
#[tokio::test]
async fn an_event_emitted_as_cpi_and_log_is_returned_once() {
    let parser = pump_fun::InstructionParser;

    let FixtureData::Instructions(fixture) = load_fixture(BUY_TX, &parser).await.unwrap() else {
        panic!("expected a transaction fixture");
    };

    // The fixture lists the self-CPI next to the buy, but the runtime nests it.
    let [buy, event_cpi] = fixture.instructions.as_slice() else {
        panic!("expected the buy and its event CPI");
    };

    let mut buy = buy.clone();
    buy.inner.push(event_cpi.clone());

    let output = parser.parse(&InstructionUpdate::from(&buy)).await.unwrap();

    assert_eq!(output.program_events.len(), 1);

    let pump_fun::event::Event::TradeEvent { args, .. } = &output.program_events[0].event else {
        panic!("expected a TradeEvent");
    };

    assert_eq!(args.mint, p("Pci4jJuQ2BUTntkTCGkv5sXEYirFm5sUefAPsBHpump"));
    assert_eq!(args.sol_amount, 243_195_286);
    assert_eq!(args.token_amount, 2_045_865_772_067);
    assert!(args.is_buy);
}
