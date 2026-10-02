use prost::Message;
use shipstern_core::Parser;
use shipstern_mock::{account_fixture, tx_fixture};
use shipstern_proc_macro::include_shipstern_parser;
use shipstern_test_utils::{check_protobuf_format, p};

include_shipstern_parser!("../idls/spl_governance.json");

#[test]
fn check_protobuf_schema() {
    check_protobuf_format(spl_governance::PROTOBUF_SCHEMA);

    insta::assert_snapshot!(
        shipstern_test_utils::normalize_protobuf_schema_for_snapshot(
            spl_governance::PROTOBUF_SCHEMA
        )
    );
}

#[tokio::test]
async fn parse_cast_vote_ix() {
    let parser = spl_governance::InstructionParser;

    let ixs = tx_fixture!(
        "eVwmoRFFUwqqcak7vF18fi7jLw1puwkn7cEWFEy8MAKYAVayAH2U4Yb9BzvJkcNpprcqkzsGWMHtRTUHTxqyCJ1",
        &parser
    );

    let (accounts, args) = ixs
        .iter()
        .find_map(|ix| match &ix.as_ref()?.instruction {
            spl_governance::instruction::Instruction::CastVote { accounts, args } => {
                Some((accounts, args))
            },
            _ => None,
        })
        .expect("no CastVote found");

    assert_eq!(accounts, &spl_governance::instruction::CastVoteAccounts {
        realm: p("3YADdZuLqfZ8ZHnxDNMnMs77qbVdhioe6yi3b4i3hfNA"),
        governance: p("AZQNzTK3KHW27S3BAyRhx8fSiyH9s5TioaEwHGHa6DPk"),
        proposal: p("BnHj4jH3kiYtBvDHsyQu7fQoqumvmEQXGiUtG7B5Nb5V"),
        proposal_owner_record: p("FDF1VFkXsBzfSF9D119EWtVmWFCxvdPGaXGJTbmZdrEX"),
        voter_token_owner_record: p("FnADdgaj1oBmKY9VgBJQSsr32kk8dNKnowcMCaqcGdKB"),
        governance_authority: p("MiLSTQNcHDmZ1cHTo7fC5kvUYMFDBuEDk2HQ2hAGs3Y"),
        vote_record: p("2ut1G4mjn4nRATPSECyXR3ZBa9qV9KyjMWBiSxdFxsGR"),
        vote_governing_token_mint: p("v3b7hZDtSvFiZuYPe71ZA13ZgijcfoksT6NZRrProoc"),
        payer: p("MiLSTQNcHDmZ1cHTo7fC5kvUYMFDBuEDk2HQ2hAGs3Y"),
        system_program: p("11111111111111111111111111111111"),
        realm_config: p("ECbYUKF92QGzSBhyhyNSR1LjDhdwG94mXTAvbMEdDPtF"),
        voter_weight_record: Some(p("FnADdgaj1oBmKY9VgBJQSsr32kk8dNKnowcMCaqcGdKB")),
        max_voter_weight_record: None,
        remaining_accounts: vec![],
    });

    assert_eq!(args, &spl_governance::instruction::CastVoteArgs {
        vote: spl_governance::Vote {
            kind: spl_governance::vote::Kind::Approve(spl_governance::VoteApprove {
                item_0: vec![spl_governance::VoteChoice {
                    rank: 0,
                    weight_percentage: 100,
                }],
            }),
        },
    });
}

#[tokio::test]
async fn parse_execute_transaction_ix() {
    let parser = spl_governance::InstructionParser;

    let ixs = tx_fixture!(
        "3WUyNrFiDCV9g8yHgftMDYCN2j2CP2E6hkAGqKSnhK9QJUGFofhgUzvE712Ya3u92kioRJ1zsdkWAAC6CaJrcFKN",
        &parser
    );

    let (accounts, args) = ixs
        .iter()
        .find_map(|ix| match &ix.as_ref()?.instruction {
            spl_governance::instruction::Instruction::ExecuteTransaction { accounts, args } => {
                Some((accounts, args))
            },
            _ => None,
        })
        .expect("no ExecuteTransaction found");

    assert_eq!(
        accounts,
        &spl_governance::instruction::ExecuteTransactionAccounts {
            governance: p("5Nn7GMBt1YrPWuzZ7nwY8kBsLkaUhCunnQM9nYaD1MZm"),
            proposal: p("2vb2Gu5reR1CNeoCSKibZSZvYFyRLRGu4xseB8HQPfvm"),
            proposal_transaction: p("Ho6j7WH8rqeWu3rsZE5dJhs2d1xAn7cJqsUyCjHjzgCz"),
            instruction_program: p("GovER5Lthms3bLBqWub97yVrMmEogzX7xNjdXpPPCVZw"),
            remaining_accounts: Some(p("5Nn7GMBt1YrPWuzZ7nwY8kBsLkaUhCunnQM9nYaD1MZm")),
        }
    );

    assert_eq!(
        args,
        &spl_governance::instruction::ExecuteTransactionArgs {}
    );

    let (set_accounts, set_args) = ixs
        .iter()
        .find_map(|ix| match &ix.as_ref()?.instruction {
            spl_governance::instruction::Instruction::SetGovernanceConfig { accounts, args } => {
                Some((accounts, args))
            },
            _ => None,
        })
        .expect("no SetGovernanceConfig found");

    assert_eq!(
        set_accounts,
        &spl_governance::instruction::SetGovernanceConfigAccounts {
            governance: p("5Nn7GMBt1YrPWuzZ7nwY8kBsLkaUhCunnQM9nYaD1MZm"),
            remaining_accounts: vec![],
        }
    );

    assert_eq!(
        set_args,
        &spl_governance::instruction::SetGovernanceConfigArgs {
            config: spl_governance::GovernanceConfig {
                community_vote_threshold: spl_governance::VoteThreshold {
                    kind: spl_governance::vote_threshold::Kind::YesVotePercentage(
                        spl_governance::VoteThresholdYesVotePercentage { item_0: 20 },
                    ),
                },
                min_community_weight_to_create_proposal: 10_000_000_000_000,
                transactions_hold_up_time: 0,
                voting_base_time: 259_200,
                community_vote_tipping: spl_governance::VoteTipping {
                    kind: spl_governance::vote_tipping::Kind::Disabled(
                        spl_governance::VoteTippingDisabled {},
                    ),
                },
                council_vote_threshold: spl_governance::VoteThreshold {
                    kind: spl_governance::vote_threshold::Kind::YesVotePercentage(
                        spl_governance::VoteThresholdYesVotePercentage { item_0: 70 },
                    ),
                },
                council_veto_vote_threshold: spl_governance::VoteThreshold {
                    kind: spl_governance::vote_threshold::Kind::YesVotePercentage(
                        spl_governance::VoteThresholdYesVotePercentage { item_0: 60 },
                    ),
                },
                min_council_weight_to_create_proposal: 1,
                council_vote_tipping: spl_governance::VoteTipping {
                    kind: spl_governance::vote_tipping::Kind::Early(
                        spl_governance::VoteTippingEarly {}
                    ),
                },
                community_veto_vote_threshold: spl_governance::VoteThreshold {
                    kind: spl_governance::vote_threshold::Kind::Disabled(
                        spl_governance::VoteThresholdDisabled {},
                    ),
                },
                voting_cool_off_time: 86_400,
                deposit_exempt_proposal_count: 10,
            },
        }
    );
}

///
/// Proto encode round-trip for instruction dispatch.
///
#[tokio::test]
async fn proto_round_trip_cast_vote_ix() {
    let parser = spl_governance::InstructionParser;

    let ixs = tx_fixture!(
        "eVwmoRFFUwqqcak7vF18fi7jLw1puwkn7cEWFEy8MAKYAVayAH2U4Yb9BzvJkcNpprcqkzsGWMHtRTUHTxqyCJ1",
        &parser
    );

    let original = ixs
        .iter()
        .find_map(|ix| ix.as_ref())
        .expect("no instruction found");

    let mut buf = Vec::new();

    original.encode(&mut buf).expect("proto encode failed");

    assert!(!buf.is_empty(), "encoded bytes should not be empty");
    assert_eq!(original.encoded_len(), buf.len());
}

#[tokio::test]
async fn parse_proposal_v2_account() {
    let parser = spl_governance::AccountParser;
    let account = account_fixture!("BnHj4jH3kiYtBvDHsyQu7fQoqumvmEQXGiUtG7B5Nb5V", &parser);

    let proposal = match account.account {
        spl_governance::account::Account::ProposalV2(p) => p,
        other => panic!("Expected ProposalV2, got {:?}", other),
    };

    let expected = spl_governance::ProposalV2 {
        account_type: spl_governance::GovernanceAccountType {
            kind: spl_governance::governance_account_type::Kind::ProposalV2(
                spl_governance::GovernanceAccountTypeProposalV2 {},
            ),
        },
        governance: p("AZQNzTK3KHW27S3BAyRhx8fSiyH9s5TioaEwHGHa6DPk"),
        governing_token_mint: p("v3b7hZDtSvFiZuYPe71ZA13ZgijcfoksT6NZRrProoc"),
        state: spl_governance::ProposalState {
            kind: spl_governance::proposal_state::Kind::Succeeded(
                spl_governance::ProposalStateSucceeded {},
            ),
        },
        token_owner_record: p("FDF1VFkXsBzfSF9D119EWtVmWFCxvdPGaXGJTbmZdrEX"),
        signatories_count: 1,
        signatories_signed_off_count: 1,
        vote_type: spl_governance::VoteType {
            kind: spl_governance::vote_type::Kind::SingleChoice(
                spl_governance::VoteTypeSingleChoice {},
            ),
        },
        options: vec![spl_governance::ProposalOption {
            label: "Approve".to_string(),
            vote_weight: 21_000_000,
            vote_result: spl_governance::OptionVoteResult {
                kind: spl_governance::option_vote_result::Kind::Succeeded(
                    spl_governance::OptionVoteResultSucceeded {},
                ),
            },
            transactions_executed_count: 0,
            transactions_count: 0,
            transactions_next_index: 0,
        }],
        deny_vote_weight: Some(0),
        reserved1: 0,
        abstain_vote_weight: None,
        start_voting_at: None,
        draft_at: 1_750_086_370,
        signing_off_at: Some(1_750_086_372),
        voting_at: Some(1_750_086_372),
        voting_at_slot: Some(347_193_948),
        voting_completed_at: Some(1_750_345_572),
        executing_at: None,
        closed_at: None,
        execution_flags: spl_governance::InstructionExecutionFlags {
            kind: spl_governance::instruction_execution_flags::Kind::None(
                spl_governance::InstructionExecutionFlagsNone {},
            ),
        },
        max_vote_weight: Some(28_000_000),
        max_voting_time: None,
        vote_threshold: Some(spl_governance::VoteThreshold {
            kind: spl_governance::vote_threshold::Kind::YesVotePercentage(
                spl_governance::VoteThresholdYesVotePercentage { item_0: 60 },
            ),
        }),
        reserved: vec![0; 64],
        name: "Quorum Check 6/16 - #1".to_string(),
        description_link: "This is Dean\n\nBeen awhile since we had a vote and there are also new \
                           members to nominate. I'd like to confirm we can meet quorum with a \
                           little room to spare \n\nThere will be 3 votes over the next 2 weeks, \
                           please vote on all 3"
            .to_string(),
        veto_vote_weight: 0,
    };

    assert_eq!(proposal, expected);
}

#[test]
fn check_json_serialization() {
    // account
    let gov = spl_governance::GovernanceV2::default();
    let json_str = serde_json::to_string(&gov).expect("failed to json serialize");
    let _: spl_governance::GovernanceV2 =
        serde_json::from_str(&json_str).expect("failed to json deserialize");

    // instruction
    let action = spl_governance::SetRealmAuthorityAction::default();
    let json_str = serde_json::to_string(&action).expect("failed to json serialize");
    let _: spl_governance::SetRealmAuthorityAction =
        serde_json::from_str(&json_str).expect("failed to json deserialize");
}

///
/// Numeric discriminators are not stripped: byte 0 is re-read as the
/// `account_type` enum tag, so `DISCRIMINATOR` is only a memcmp predicate.
/// Decode with `try_unpack`.
///
#[test]
fn exposes_numeric_account_discriminators() {
    assert_eq!(spl_governance::RealmV2::DISCRIMINATOR, [16_u8].as_slice());
    assert_eq!(spl_governance::RealmV2::DISCRIMINATOR_OFFSET, 0);

    assert_eq!(
        spl_governance::ProposalV2::DISCRIMINATOR,
        [14_u8].as_slice()
    );
    assert_eq!(spl_governance::ProposalV2::DISCRIMINATOR_OFFSET, 0);
}
