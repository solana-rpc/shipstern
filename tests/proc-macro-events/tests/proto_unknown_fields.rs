use prost::Message;
use shipstern_core::Parser;
use shipstern_mock::tx_fixture;
use shipstern_proc_macro::include_shipstern_parser;

include_shipstern_parser!("../idls/perpetuals.json");

const BORROW_FROM_CUSTODY_TX: &str =
    "5mYEUYXCZisS8CChCG8mL8N3NEWHUA81Rr7kLA28P5upSzDStLq1f4QKhFLY7R8GsRNB27gM6YzvKerxejtLQxCj";

#[tokio::test]
async fn events_skip_unknown_fields() {
    let parser = perpetuals::InstructionParser;

    let ixs = tx_fixture!(BORROW_FROM_CUSTODY_TX, &parser);
    let output = ixs.iter().find_map(|out| out.as_ref()).expect("no parsed output");
    let expected = output.program_events[0].clone();

    // Fields 1000 (varint) and 1001 (length-delimited), past any variant tag.
    for foreign in [&[0xc0_u8, 0x3e, 0x01][..], &[0xca, 0x3e, 0x02, 0xaa, 0xbb][..]] {
        let mut decoded = expected.clone();

        decoded.merge(foreign).expect("unknown field is skipped");

        assert_eq!(decoded, expected);
    }
}

#[tokio::test]
async fn events_still_merge_known_fields() {
    let parser = perpetuals::InstructionParser;

    let ixs = tx_fixture!(BORROW_FROM_CUSTODY_TX, &parser);
    let output = ixs.iter().find_map(|out| out.as_ref()).expect("no parsed output");
    let expected = output.program_events[0].clone();

    let mut decoded = expected.clone();
    decoded.merge(expected.encode_to_vec().as_slice()).unwrap();

    assert_eq!(decoded, expected);
}
