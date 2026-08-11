#![cfg(test)]

use finchippay_contract::{FinchippayContract, FinchippayContractClient};
use soroban_sdk::{
    testutils::{Address as _, Ledger},
    token, Address, Env, Symbol,
};

fn deploy(env: &Env) -> (Address, FinchippayContractClient<'_>) {
    let id = env.register(FinchippayContract, ());
    let client = FinchippayContractClient::new(env, &id);
    let admin = Address::generate(env);
    client.initialize(&admin);
    (id, client)
}

fn create_token(env: &Env, admin: &Address, to: &Address, amount: i128) -> Address {
    let sac_contract = env.register_stellar_asset_contract_v2(admin.clone());
    let token_id = sac_contract.address();
    let sac = token::StellarAssetClient::new(env, &token_id);
    sac.mint(to, &amount);
    token_id
}

/// Helper: open a stream, advance some ledgers, and return (client, stream_id, payer, recipient, token).
fn setup_stream(
    env: &Env,
    client: &FinchippayContractClient<'_>,
) -> (u32, Address, Address, Address) {
    let payer = Address::generate(env);
    let recipient = Address::generate(env);
    let admin = client.get_admin();

    // Mint a generous amount so we don't hit deposit limits.
    let token_id = create_token(env, &admin, &payer, 1_000_000_000_000);

    let stream_id = client.open_stream(&token_id, &payer, &recipient, &100, &500_000);
    (stream_id, payer, recipient, token_id)
}

// ─── Helper: basic claim ─────────────────────────────────────────────────

#[test]
fn test_pause_and_resume_claiming_freezes() {
    let env = Env::default();
    let (_, client) = deploy(&env);
    env.mock_all_auths();

    let (stream_id, _payer, recipient, _token) = setup_stream(&env, &client);

    // Advance 50 ledgers and claim.
    env.ledger().set_sequence_number(env.ledger().sequence() + 50);
    let first_claim = client.claim_stream(&stream_id, &recipient);
    // rate=100, 50 ledgers => 5000 claimable
    assert_eq!(first_claim, 5_000);

    // Pause the stream with no auto-resume.
    client.pause_stream_by_recipient(
        &stream_id,
        &recipient,
        &None::<u32>,
        &Symbol::new(&env, "kyc_review"),
    );

    // Verify pause info.
    let (is_paused, paused_at, _duration, auto_resume, reason) =
        client.get_stream_pause_info(&stream_id);
    assert!(is_paused);
    assert_eq!(paused_at, env.ledger().sequence());
    assert_eq!(auto_resume, 0);
    assert_eq!(reason, Symbol::new(&env, "kyc_review"));

    // Advance 30 more ledgers. While paused, no new tokens should be claimable.
    env.ledger().set_sequence_number(env.ledger().sequence() + 30);
    let claim_while_paused = client.claim_stream(&stream_id, &recipient);
    assert_eq!(claim_while_paused, 0);

    // Resume the stream.
    client.resume_stream_by_recipient(&stream_id, &recipient);

    // After resume, pause info should be cleared.
    let (is_paused2, paused_at2, duration2, auto_resume2, _) =
        client.get_stream_pause_info(&stream_id);
    assert!(!is_paused2);
    assert_eq!(paused_at2, 0);
    assert_eq!(auto_resume2, 0);
    // 30 ledgers of pause duration accumulated.
    assert_eq!(duration2, 30);

    // Advance 20 ledgers after resume.
    env.ledger().set_sequence_number(env.ledger().sequence() + 20);
    let second_claim = client.claim_stream(&stream_id, &recipient);
    // rate=100, 20 ledgers => 2000 claimable.
    assert_eq!(second_claim, 2_000);
}

// ─── Auto-resume test ────────────────────────────────────────────────────

#[test]
fn test_auto_resume_on_deadline() {
    let env = Env::default();
    let (_, client) = deploy(&env);
    env.mock_all_auths();

    let (stream_id, _payer, recipient, _token) = setup_stream(&env, &client);

    // Advance 10 ledgers, claim.
    env.ledger().set_sequence_number(env.ledger().sequence() + 10);
    let first_claim = client.claim_stream(&stream_id, &recipient);
    assert_eq!(first_claim, 1_000);

    // Pause with auto-resume at current + 50 ledgers.
    let current = env.ledger().sequence();
    let auto_resume = current + 50;
    client.pause_stream_by_recipient(
        &stream_id,
        &recipient,
        &Some(auto_resume),
        &Symbol::new(&env, "travel"),
    );

    // Advance past the auto-resume deadline.
    // start=0, first claim at ledger 10 (1000 claimed).
    // paused at ledger 10, auto_resume=60. Now jump to ledger 65.
    env.ledger().set_sequence_number(auto_resume + 5);

    // claim_stream should auto-resume, but no new tokens are claimable yet
    // because effective elapsed = 65-0-55 = 10, and we already claimed 1000.
    let claim_after_auto = client.claim_stream(&stream_id, &recipient);
    assert_eq!(claim_after_auto, 0);

    // Pause should be cleared after auto-resume.
    let (is_paused, _, _, _, _) = client.get_stream_pause_info(&stream_id);
    assert!(!is_paused);

    // Advance another 10 ledgers and claim normally.
    env.ledger().set_sequence_number(env.ledger().sequence() + 10);
    let third_claim = client.claim_stream(&stream_id, &recipient);
    // effective elapsed = 75-0-55 = 20, total_streamed = 2000, claimed=1000, claimable=1000.
    assert_eq!(third_claim, 1_000);
}

// ─── Pause rejections ────────────────────────────────────────────────────

#[test]
#[should_panic(expected = "stream is already paused by recipient")]
fn test_cannot_pause_already_paused_stream() {
    let env = Env::default();
    let (_, client) = deploy(&env);
    env.mock_all_auths();

    let (stream_id, _payer, recipient, _token) = setup_stream(&env, &client);

    // First pause.
    client.pause_stream_by_recipient(
        &stream_id,
        &recipient,
        &None::<u32>,
        &Symbol::new(&env, "test"),
    );

    // Second pause should panic.
    client.pause_stream_by_recipient(
        &stream_id,
        &recipient,
        &None::<u32>,
        &Symbol::new(&env, "test"),
    );
}

#[test]
#[should_panic(expected = "stream is not paused by recipient")]
fn test_cannot_resume_non_paused_stream() {
    let env = Env::default();
    let (_, client) = deploy(&env);
    env.mock_all_auths();

    let (stream_id, _payer, recipient, _token) = setup_stream(&env, &client);

    // Resume without pausing first should panic.
    client.resume_stream_by_recipient(&stream_id, &recipient);
}

#[test]
#[should_panic(expected = "auto_resume_ledger is too far in the future")]
fn test_pause_beyond_max_ledgers_rejected() {
    let env = Env::default();
    let (_, client) = deploy(&env);
    env.mock_all_auths();

    let (stream_id, _payer, recipient, _token) = setup_stream(&env, &client);

    let far_future = env.ledger().sequence() + 10_000_000; // exceeds MAX_PAUSE_LEDGERS
    client.pause_stream_by_recipient(
        &stream_id,
        &recipient,
        &Some(far_future),
        &Symbol::new(&env, "test"),
    );
}

#[test]
#[should_panic(expected = "auto_resume_ledger must be in the future")]
fn test_pause_with_past_auto_resume_rejected() {
    let env = Env::default();
    let (_, client) = deploy(&env);
    env.mock_all_auths();

    let (stream_id, _payer, recipient, _token) = setup_stream(&env, &client);

    let past = env.ledger().sequence(); // not strictly greater
    client.pause_stream_by_recipient(
        &stream_id,
        &recipient,
        &Some(past),
        &Symbol::new(&env, "test"),
    );
}

// ─── Interaction with other operations ───────────────────────────────────

#[test]
fn test_top_up_while_paused() {
    let env = Env::default();
    let (_, client) = deploy(&env);
    env.mock_all_auths();

    let (stream_id, payer, recipient, _token_id) = setup_stream(&env, &client);

    // Pause the stream.
    client.pause_stream_by_recipient(
        &stream_id,
        &recipient,
        &None::<u32>,
        &Symbol::new(&env, "test"),
    );

    // Top up should still succeed.
    client.top_up_stream(&stream_id, &payer, &100_000);

    let stream = client.get_stream(&stream_id);
    assert_eq!(stream.deposited, 600_000); // 500_000 + 100_000
    assert!(stream.recipient_paused);
}

#[test]
fn test_close_stream_while_paused() {
    let env = Env::default();
    let (_, client) = deploy(&env);
    env.mock_all_auths();

    let (stream_id, payer, recipient, _token) = setup_stream(&env, &client);

    // Advance 20 ledgers, then pause.
    env.ledger().set_sequence_number(env.ledger().sequence() + 20);
    client.pause_stream_by_recipient(
        &stream_id,
        &recipient,
        &None::<u32>,
        &Symbol::new(&env, "test"),
    );

    // Advance 10 ledgers while paused, then payer closes.
    env.ledger().set_sequence_number(env.ledger().sequence() + 10);
    let refund = client.close_stream(&stream_id, &payer);

    // The recipient should only get claimable tokens from before the pause.
    // 20 ledgers * 100 rate = 2000. Remaining deposited = 500_000 - 2000 = 498_000.
    assert_eq!(refund, 498_000);

    let stream = client.get_stream(&stream_id);
    assert!(stream.closed);
}

#[test]
fn test_reject_stream_while_paused() {
    let env = Env::default();
    let (_, client) = deploy(&env);
    env.mock_all_auths();

    let (stream_id, _payer, recipient, _token) = setup_stream(&env, &client);

    // Advance 20 ledgers, then pause.
    env.ledger().set_sequence_number(env.ledger().sequence() + 20);
    client.pause_stream_by_recipient(
        &stream_id,
        &recipient,
        &None::<u32>,
        &Symbol::new(&env, "test"),
    );

    // Advance 10 ledgers while paused, then recipient rejects.
    env.ledger().set_sequence_number(env.ledger().sequence() + 10);
    let refund = client.reject_stream(&stream_id, &recipient);

    // Refund: 500_000 - 2000 (accrued before pause) = 498_000.
    assert_eq!(refund, 498_000);

    let stream = client.get_stream(&stream_id);
    assert!(stream.closed);
}

#[test]
fn test_transfer_stream_clears_pause_state() {
    let env = Env::default();
    let (_, client) = deploy(&env);
    env.mock_all_auths();

    let (stream_id, _payer, recipient, _token) = setup_stream(&env, &client);

    // Pause with auto-resume.
    client.pause_stream_by_recipient(
        &stream_id,
        &recipient,
        &Some(env.ledger().sequence() + 100),
        &Symbol::new(&env, "travel"),
    );

    let new_recipient = Address::generate(&env);
    client.transfer_stream(&stream_id, &recipient, &new_recipient);

    let stream = client.get_stream(&stream_id);
    assert_eq!(stream.recipient, new_recipient);
    // Pause state should be cleared (duration accumulated) — the new recipient
    // can choose to pause again.
    assert!(!stream.recipient_paused);
}

// ─── Edge cases ──────────────────────────────────────────────────────────

#[test]
fn test_pause_with_auto_resume_at_max_boundary() {
    let env = Env::default();
    let (_, client) = deploy(&env);
    env.mock_all_auths();

    let (stream_id, _payer, recipient, _token) = setup_stream(&env, &client);

    // Exactly MAX_PAUSE_LEDGERS in the future — should succeed.
    let max_boundary = env.ledger().sequence() + 6_307_200;
    client.pause_stream_by_recipient(
        &stream_id,
        &recipient,
        &Some(max_boundary),
        &Symbol::new(&env, "compliance"),
    );

    let (is_paused, _, _, auto_resume, _) = client.get_stream_pause_info(&stream_id);
    assert!(is_paused);
    assert_eq!(auto_resume, max_boundary);
}

#[test]
fn test_claimable_does_not_change_post_pause() {
    let env = Env::default();
    let (_, client) = deploy(&env);
    env.mock_all_auths();

    let (stream_id, _payer, recipient, _token) = setup_stream(&env, &client);

    // Advance 10 ledgers.
    env.ledger().set_sequence_number(env.ledger().sequence() + 10);
    let claimable_before = client.get_claimable(&stream_id);

    // Pause.
    client.pause_stream_by_recipient(
        &stream_id,
        &recipient,
        &None::<u32>,
        &Symbol::new(&env, "test"),
    );

    // Advance 50 ledgers while paused — claimable should not change.
    env.ledger().set_sequence_number(env.ledger().sequence() + 50);
    let claimable_after = client.get_claimable(&stream_id);
    assert_eq!(claimable_after, claimable_before);

    // Resume.
    client.resume_stream_by_recipient(&stream_id, &recipient);

    // Advance 5 ledgers — claimable should increase by 5*100 = 500.
    env.ledger().set_sequence_number(env.ledger().sequence() + 5);
    let claimable_resumed = client.get_claimable(&stream_id);
    assert_eq!(claimable_resumed, claimable_before + 500);
}

#[test]
fn test_only_recipient_can_pause_or_resume() {
    let env = Env::default();
    let (_, client) = deploy(&env);
    env.mock_all_auths();

    let (stream_id, payer, _recipient, _token) = setup_stream(&env, &client);

    // The payer is NOT the recipient — pause by payer should be caught by auth
    // since we mock_all_auths, but the stream logic checks `stream.recipient != recipient`.
    // We test by calling as someone other than the recipient.

    // For this test, we need to verify the auth guard works.
    // Actually, mock_all_auths skips auth checks. The stream-level check
    // `stream.recipient != recipient` will catch this.
    // Call pause as the payer (who is not the recipient).
    // Since this is an on-chain check, the function will panic with
    // "only the recipient may pause".
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        client.pause_stream_by_recipient(
            &stream_id,
            &payer, // payer is NOT the recipient
            &None::<u32>,
            &Symbol::new(&env, "test"),
        );
    }));
    assert!(result.is_err());
}

#[test]
fn test_pause_info_on_unpaused_stream_returns_zeros() {
    let env = Env::default();
    let (_, client) = deploy(&env);
    env.mock_all_auths();

    let (stream_id, _payer, _recipient, _token) = setup_stream(&env, &client);

    let (is_paused, paused_at, duration, auto_resume, reason) =
        client.get_stream_pause_info(&stream_id);
    assert!(!is_paused);
    assert_eq!(paused_at, 0);
    assert_eq!(duration, 0);
    assert_eq!(auto_resume, 0);
    assert_eq!(reason, Symbol::new(&env, ""));
}

#[test]
fn test_multiple_pause_resume_cycles() {
    let env = Env::default();
    let (_, client) = deploy(&env);
    env.mock_all_auths();

    let (stream_id, _payer, recipient, _token) = setup_stream(&env, &client);

    // Cycle 1
    env.ledger().set_sequence_number(env.ledger().sequence() + 10);
    client.pause_stream_by_recipient(
        &stream_id,
        &recipient,
        &None::<u32>,
        &Symbol::new(&env, "cycle1"),
    );
    env.ledger().set_sequence_number(env.ledger().sequence() + 20);
    client.resume_stream_by_recipient(&stream_id, &recipient);

    // Cycle 2
    env.ledger().set_sequence_number(env.ledger().sequence() + 15);
    client.pause_stream_by_recipient(
        &stream_id,
        &recipient,
        &None::<u32>,
        &Symbol::new(&env, "cycle2"),
    );
    env.ledger().set_sequence_number(env.ledger().sequence() + 10);
    client.resume_stream_by_recipient(&stream_id, &recipient);

    // Total pause: 20 + 10 = 30 ledgers.
    let (_is_paused, _paused_at, total_duration, _auto_resume, _) =
        client.get_stream_pause_info(&stream_id);
    assert_eq!(total_duration, 30);

    // Advance 10 ledgers after resuming.
    env.ledger().set_sequence_number(env.ledger().sequence() + 10);
    let claimable = client.claim_stream(&stream_id, &recipient);

    // Effective elapsed: (10+20+15+10+10) = 65 ledgers from start after resuming all
    // Actually, let's trace: start at ledger 1.
    // Cycle1 pause: advanced 10, paused 20, resumed.
    // Cycle2 pause: advanced 15, paused 10, resumed. 
    // Then advanced 10, claimed.
    // Total ledgers since start: 10 + 20 + 15 + 10 + 10 = 65.
    // Total pause: 30.
    // Effective: 65 - 30 = 35.
    // rate=100 => 3500 claimable.
    assert_eq!(claimable, 3_500);
}

#[test]
fn test_auto_resume_preserves_pause_duration() {
    let env = Env::default();
    let (_, client) = deploy(&env);
    env.mock_all_auths();

    let (stream_id, _payer, recipient, _token) = setup_stream(&env, &client);

    // Advance 5 ledgers and claim.
    env.ledger().set_sequence_number(env.ledger().sequence() + 5);
    let _ = client.claim_stream(&stream_id, &recipient);

    // Pause with auto-resume in 40 ledgers.
    let auto_resume = env.ledger().sequence() + 40;
    client.pause_stream_by_recipient(
        &stream_id,
        &recipient,
        &Some(auto_resume),
        &Symbol::new(&env, "auto"),
    );

    // Jump past deadline.
    env.ledger().set_sequence_number(auto_resume + 10);

    // Claim — auto-resume fires.
    let claim_amount = client.claim_stream(&stream_id, &recipient);

    // After auto-resume, pause duration should reflect actual elapsed pause.
    // start=0, paused at ledger 5, auto_resume=45, now at ledger 55.
    // Actual pause = 55 - 5 = 50 ledgers.
    let (_is_paused, _paused_at, duration, _auto, _) =
        client.get_stream_pause_info(&stream_id);
    assert_eq!(duration, 50);

    // effective elapsed = 55 - 0 - 50 = 5 ledgers.
    // total_streamed = 100 * 5 = 500, claimed so far = 500, claimable = 0.
    assert_eq!(claim_amount, 0);

    // Advance 10 more ledgers; effective elapsed = 65 - 0 - 50 = 15.
    // total_streamed = 1500, claimed = 500, claimable = 1000.
    env.ledger().set_sequence_number(env.ledger().sequence() + 10);
    let second_claim = client.claim_stream(&stream_id, &recipient);
    assert_eq!(second_claim, 1_000);
}
