//! Anchor converters leave event payload types out of `definedTypes`, so a link
//! to one must still resolve to the payload struct (#335, #336).

use shipstern_proc_macro::include_shipstern_parser;

include_shipstern_parser!("../idls/anchor/event_payload_links.anchor.json");

#[test]
fn links_to_event_payloads_decode_the_payload() {
    let mut data = event_payload_links::Instructions::COMMIT_DISCRIMINATOR.to_vec();
    data.extend_from_slice(&11_u64.to_le_bytes());
    data.push(0);
    data.extend_from_slice(&99_u64.to_le_bytes());

    let path = shipstern_core::instruction::Path::new_single(0);
    let parsed = event_payload_links::resolve_instruction_default(&[], &data, &path)
        .expect("commit discriminator should match");

    let event_payload_links::instruction::Instruction::Commit { args, .. } = parsed.instruction;
    let event_payload_links::market_event::Kind::Header(header) = args.market_event.kind;

    assert_eq!(args.authority, event_payload_links::VrfAuthority { x: 11 });
    assert_eq!(header.item_0, event_payload_links::MarketEventHeader {
        slot: 99
    });
}
