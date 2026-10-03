//! An event field typed as another event's payload holds that payload, not the
//! other event's `{ accounts, args }` wrapper, in Rust and in the proto schema.

use shipstern_proc_macro::include_shipstern_parser;

include_shipstern_parser!("../idls/anchor/event_payload_in_event.anchor.json");

#[test]
fn an_event_field_typed_as_another_event_decodes_its_payload() {
    let mut data = vec![8; 8];
    data.extend_from_slice(&5_u64.to_le_bytes());
    data.extend_from_slice(&42_u64.to_le_bytes());

    let parsed = event_payload_in_event::resolve_event_default(&[], &data)
        .expect("Outer discriminator should match");

    let event_payload_in_event::event::Event::Outer { args, .. } = parsed.event else {
        panic!("expected Outer");
    };

    assert_eq!(args.inner, event_payload_in_event::Inner { x: 5 });
    assert_eq!(args.tail, 42);

    assert!(
        event_payload_in_event::PROTOBUF_SCHEMA.contains("message OuterArgs {\n  Inner inner = 1;")
    );
}
