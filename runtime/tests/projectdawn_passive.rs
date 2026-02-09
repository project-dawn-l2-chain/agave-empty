#![allow(clippy::arithmetic_side_effects)]

use {
    solana_accounts_db::epoch_accounts_hash::EpochAccountsHash,
    solana_runtime::{
        bank::Bank,
        bank_client::BankClient,
        bank_forks::BankForks,
        genesis_utils::{create_genesis_config_with_leader, GenesisConfigInfo},
        projectdawn_config::DEFAULT_TREASURY_PUBKEY,
    },
    solana_sdk::{
        account::ReadableAccount,
        account_utils::StateMut,
        client::SyncClient,
        clock::Slot,
        epoch_schedule::{EpochSchedule, MINIMUM_SLOTS_PER_EPOCH},
        hash::Hash,
        message::Message,
        pubkey::Pubkey,
        rent::Rent,
        signature::{Keypair, Signer},
        stake::{
            instruction as stake_instruction,
            state::{Authorized, Lockup, StakeStateV2},
        },
        system_program,
    },
    solana_stake_interface::instruction as pd_stake_instruction,
    solana_vote_program::{
        vote_instruction,
        vote_state::{VoteInit, VoteState, VoteStateVersions},
    },
    std::sync::{Arc, RwLock},
};

fn new_bank_from_parent_with_bank_forks(
    bank_forks: &RwLock<BankForks>,
    parent: Arc<Bank>,
    collector_id: &Pubkey,
    slot: Slot,
) -> Arc<Bank> {
    let bank = Bank::new_from_parent(parent, collector_id, slot);
    bank_forks
        .write()
        .unwrap()
        .insert(bank)
        .clone_without_scheduler()
}

/// get bank at next epoch + `n` slots
fn next_epoch_and_n_slots(
    bank: Arc<Bank>,
    bank_forks: &RwLock<BankForks>,
    mut n: usize,
) -> Arc<Bank> {
    bank.squash();
    let slot = bank.get_slots_in_epoch(bank.epoch()) + bank.slot();
    let mut bank = new_bank_from_parent_with_bank_forks(bank_forks, bank, &Pubkey::default(), slot);

    while n > 0 {
        bank.squash();
        let slot = bank.slot() + 1;
        bank = new_bank_from_parent_with_bank_forks(bank_forks, bank, &Pubkey::default(), slot);
        n -= 1;
    }

    bank
}

// ---------------------------------------------------------------------------
// Test 1: PassiveLock(3) creates correct stake state
// ---------------------------------------------------------------------------
#[test]
fn test_passive_lock_e2e() {
    solana_logger::setup();

    let stake_keypair = Keypair::new();
    let stake_pubkey = stake_keypair.pubkey();

    let GenesisConfigInfo {
        genesis_config,
        mint_keypair,
        ..
    } = create_genesis_config_with_leader(
        100_000_000_000,
        &solana_pubkey::new_rand(),
        1_000_000,
    );

    let (bank, _bank_forks) = Bank::new_with_bank_forks_for_tests(&genesis_config);
    let bank_client = BankClient::new_shared(bank.clone());
    let mint_pubkey = mint_keypair.pubkey();

    let rent = bank.rent_collector().rent.clone();
    let stake_rent_exempt_reserve = rent.minimum_balance(StakeStateV2::size_of());
    let stake_lamports = 10_000_000_000; // 10 SOL total

    // Create an Initialized stake account (no delegation)
    let authorized = Authorized::auto(&stake_pubkey);
    let message = Message::new(
        &stake_instruction::create_account(
            &mint_pubkey,
            &stake_pubkey,
            &authorized,
            &Lockup::default(),
            stake_lamports,
        ),
        Some(&mint_pubkey),
    );
    bank_client
        .send_and_confirm_message(&[&mint_keypair, &stake_keypair], message)
        .expect("failed to create stake account");

    // Verify Initialized state
    let account = bank.get_account(&stake_pubkey).expect("account not found");
    let state: StakeStateV2 = account.state().expect("couldn't unpack account data");
    assert!(matches!(state, StakeStateV2::Initialized(_)));

    // Send PassiveLock(3)
    let message = Message::new(
        &[pd_stake_instruction::passive_lock(
            &stake_pubkey,
            &stake_pubkey, // staker == stake_keypair
            3,
        )],
        Some(&mint_pubkey),
    );
    bank_client
        .send_and_confirm_message(&[&mint_keypair, &stake_keypair], message)
        .expect("failed to passive lock");

    // Verify resulting state
    let account = bank.get_account(&stake_pubkey).expect("account not found");
    let state: StakeStateV2 = account.state().expect("couldn't unpack account data");
    if let StakeStateV2::Stake(meta, stake, flags) = state {
        assert_eq!(flags.passive_tier(), 3, "tier should be 3");
        assert_eq!(
            stake.delegation.voter_pubkey,
            Pubkey::default(),
            "passive stake sentinel voter"
        );
        // Tier 3 lockup = 36 epochs from current epoch
        let current_epoch = bank.epoch();
        assert_eq!(
            meta.lockup.epoch,
            current_epoch + 36,
            "lockup should be current_epoch + 36"
        );
        // Stake amount should be total - rent_exempt_reserve
        assert_eq!(
            stake.delegation.stake,
            stake_lamports - stake_rent_exempt_reserve
        );
    } else {
        panic!("expected StakeStateV2::Stake, got {:?}", state);
    }
}

// ---------------------------------------------------------------------------
// Test 2: Lockup is enforced — cannot withdraw before expiry
//
// Demonstrates that:
//   (a) During lockup: withdraw fails (LockupInForce)
//   (b) After lockup:  early_unlock succeeds (proving lockup no longer blocks),
//       which resets account to Initialized, after which withdraw succeeds.
// ---------------------------------------------------------------------------
#[test]
fn test_passive_lock_lockup_enforced() {
    solana_logger::setup();

    let stake_keypair = Keypair::new();
    let stake_pubkey = stake_keypair.pubkey();

    let GenesisConfigInfo {
        mut genesis_config,
        mint_keypair,
        ..
    } = create_genesis_config_with_leader(
        100_000_000_000,
        &solana_pubkey::new_rand(),
        2_000_000_000,
    );
    genesis_config.epoch_schedule = EpochSchedule::new(MINIMUM_SLOTS_PER_EPOCH);
    genesis_config.rent = Rent::default();

    // Seed treasury for early_unlock
    genesis_config.accounts.insert(
        DEFAULT_TREASURY_PUBKEY,
        solana_sdk::account::Account::new(1_000_000_000, 0, &system_program::id()),
    );

    let (bank, bank_forks) = Bank::new_with_bank_forks_for_tests(&genesis_config);
    // Set EAH to Valid so new_from_parent doesn't panic during freeze
    bank.rc
        .accounts
        .accounts_db
        .epoch_accounts_hash_manager
        .set_valid(EpochAccountsHash::new(Hash::new_unique()), bank.slot());

    let bank_client = BankClient::new_shared(bank.clone());
    let mint_pubkey = mint_keypair.pubkey();

    let stake_lamports = 10_000_000_000;

    // Create Initialized stake
    let authorized = Authorized::auto(&stake_pubkey);
    let message = Message::new(
        &stake_instruction::create_account(
            &mint_pubkey,
            &stake_pubkey,
            &authorized,
            &Lockup::default(),
            stake_lamports,
        ),
        Some(&mint_pubkey),
    );
    bank_client
        .send_and_confirm_message(&[&mint_keypair, &stake_keypair], message)
        .expect("failed to create stake account");

    // PassiveLock(3) at epoch 0 — lockup until epoch 36
    let message = Message::new(
        &[pd_stake_instruction::passive_lock(
            &stake_pubkey,
            &stake_pubkey,
            3,
        )],
        Some(&mint_pubkey),
    );
    bank_client
        .send_and_confirm_message(&[&mint_keypair, &stake_keypair], message)
        .expect("failed to passive lock");

    // Advance a few epochs (still within lockup)
    let bank = next_epoch_and_n_slots(bank, &bank_forks, 0);
    let bank = next_epoch_and_n_slots(bank, &bank_forks, 0);
    assert!(bank.epoch() < 36, "should still be before lockup expiry");

    // Part (a): Attempt to withdraw 1 lamport — should fail (LockupInForce)
    let bank_client = BankClient::new_shared(bank.clone());
    let recipient = solana_pubkey::new_rand();
    let message = Message::new(
        &[stake_instruction::withdraw(
            &stake_pubkey,
            &stake_pubkey, // withdrawer
            &recipient,
            1, // try to withdraw just 1 lamport
            None,
        )],
        Some(&mint_pubkey),
    );
    let result = bank_client.send_and_confirm_message(&[&mint_keypair, &stake_keypair], message);
    assert!(result.is_err(), "withdraw should fail during lockup period");

    // Advance past epoch 36 with extra slots to clear the epoch rewards period
    let mut bank = bank;
    while bank.epoch() <= 36 {
        bank = next_epoch_and_n_slots(bank, &bank_forks, 2);
    }
    assert!(bank.epoch() > 36, "should be past lockup expiry");

    // Part (b): Use early_unlock to prove lockup is no longer blocking.
    // early_unlock resets to Initialized with Lockup::default() and deducts penalty.
    let bank_client = BankClient::new_shared(bank.clone());
    let message = Message::new(
        &[pd_stake_instruction::early_unlock(
            &stake_pubkey,
            &stake_pubkey,
            &DEFAULT_TREASURY_PUBKEY,
        )],
        Some(&mint_pubkey),
    );
    bank_client
        .send_and_confirm_message(&[&mint_keypair, &stake_keypair], message)
        .expect("early_unlock should succeed after lockup expires");

    // After early_unlock, account is Initialized with no lockup — withdraw should work
    let actual_balance = bank.get_account(&stake_pubkey).expect("account").lamports();
    let bank_client = BankClient::new_shared(bank.clone());
    let message = Message::new(
        &[stake_instruction::withdraw(
            &stake_pubkey,
            &stake_pubkey,
            &recipient,
            actual_balance,
            None,
        )],
        Some(&mint_pubkey),
    );
    assert!(
        bank_client
            .send_and_confirm_message(&[&mint_keypair, &stake_keypair], message)
            .is_ok(),
        "withdraw should succeed after early_unlock clears lockup"
    );
}

// ---------------------------------------------------------------------------
// Test 3: Early unlock deducts penalty to treasury
// ---------------------------------------------------------------------------
#[test]
fn test_early_unlock_penalty_to_treasury() {
    solana_logger::setup();

    let stake_keypair = Keypair::new();
    let stake_pubkey = stake_keypair.pubkey();

    let GenesisConfigInfo {
        mut genesis_config,
        mint_keypair,
        ..
    } = create_genesis_config_with_leader(
        100_000_000_000,
        &solana_pubkey::new_rand(),
        1_000_000,
    );

    // Seed the treasury account so it exists on-chain
    genesis_config.accounts.insert(
        DEFAULT_TREASURY_PUBKEY,
        solana_sdk::account::Account::new(1_000_000_000, 0, &system_program::id()),
    );

    let (bank, _bank_forks) = Bank::new_with_bank_forks_for_tests(&genesis_config);
    let bank_client = BankClient::new_shared(bank.clone());
    let mint_pubkey = mint_keypair.pubkey();

    let rent = bank.rent_collector().rent.clone();
    let stake_rent_exempt_reserve = rent.minimum_balance(StakeStateV2::size_of());
    let stake_lamports = 10_000_000_000; // 10 SOL

    // Create Initialized stake
    let authorized = Authorized::auto(&stake_pubkey);
    let message = Message::new(
        &stake_instruction::create_account(
            &mint_pubkey,
            &stake_pubkey,
            &authorized,
            &Lockup::default(),
            stake_lamports,
        ),
        Some(&mint_pubkey),
    );
    bank_client
        .send_and_confirm_message(&[&mint_keypair, &stake_keypair], message)
        .expect("failed to create stake account");

    // PassiveLock(2) — tier 2, 14-epoch lockup
    let message = Message::new(
        &[pd_stake_instruction::passive_lock(
            &stake_pubkey,
            &stake_pubkey,
            2,
        )],
        Some(&mint_pubkey),
    );
    bank_client
        .send_and_confirm_message(&[&mint_keypair, &stake_keypair], message)
        .expect("failed to passive lock");

    // Record balances before early unlock
    let treasury_balance_before = bank
        .get_account(&DEFAULT_TREASURY_PUBKEY)
        .expect("treasury not found")
        .lamports();
    let stake_balance_before = bank
        .get_account(&stake_pubkey)
        .expect("stake not found")
        .lamports();

    // Compute expected penalty: tier 2, rate_bp = 50
    // penalty = stake_amount * 50 * 5 / 10_000 = stake_amount * 250 / 10_000
    let stake_amount = stake_lamports - stake_rent_exempt_reserve;
    let expected_penalty = ((stake_amount as u128) * 50 * 5 / 10_000) as u64;

    // Send EarlyUnlock
    let message = Message::new(
        &[pd_stake_instruction::early_unlock(
            &stake_pubkey,
            &stake_pubkey, // staker
            &DEFAULT_TREASURY_PUBKEY,
        )],
        Some(&mint_pubkey),
    );
    bank_client
        .send_and_confirm_message(&[&mint_keypair, &stake_keypair], message)
        .expect("failed to early unlock");

    // Verify treasury received the penalty
    let treasury_balance_after = bank
        .get_account(&DEFAULT_TREASURY_PUBKEY)
        .expect("treasury not found")
        .lamports();
    assert_eq!(
        treasury_balance_after,
        treasury_balance_before + expected_penalty,
        "treasury should receive penalty"
    );

    // Verify stake account was debited
    let stake_balance_after = bank
        .get_account(&stake_pubkey)
        .expect("stake not found")
        .lamports();
    assert_eq!(
        stake_balance_after,
        stake_balance_before - expected_penalty,
        "stake should be debited by penalty"
    );

    // Verify stake is now Initialized (not Stake) with cleared lockup
    let account = bank.get_account(&stake_pubkey).expect("account not found");
    let state: StakeStateV2 = account.state().expect("couldn't unpack account data");
    if let StakeStateV2::Initialized(meta) = state {
        assert_eq!(meta.lockup, Lockup::default(), "lockup should be cleared");
    } else {
        panic!("expected StakeStateV2::Initialized after early unlock, got {:?}", state);
    }
}

// ---------------------------------------------------------------------------
// Test 4: Early unlock rejects non-passive (delegated) stake
// ---------------------------------------------------------------------------
#[test]
fn test_early_unlock_rejects_non_passive() {
    solana_logger::setup();

    let stake_keypair = Keypair::new();
    let stake_pubkey = stake_keypair.pubkey();
    let vote_keypair = Keypair::new();
    let vote_pubkey = vote_keypair.pubkey();
    let identity_keypair = Keypair::new();
    let identity_pubkey = identity_keypair.pubkey();

    let GenesisConfigInfo {
        mut genesis_config,
        mint_keypair,
        ..
    } = create_genesis_config_with_leader(
        100_000_000_000,
        &solana_pubkey::new_rand(),
        2_000_000_000,
    );
    genesis_config.rent = Rent::default();

    // Seed treasury account
    genesis_config.accounts.insert(
        DEFAULT_TREASURY_PUBKEY,
        solana_sdk::account::Account::new(1_000_000_000, 0, &system_program::id()),
    );

    let (bank, _bank_forks) = Bank::new_with_bank_forks_for_tests(&genesis_config);
    let bank_client = BankClient::new_shared(bank.clone());
    let mint_pubkey = mint_keypair.pubkey();

    let rent = bank.rent_collector().rent.clone();
    let vote_balance = rent.minimum_balance(VoteState::size_of());

    // Create vote account
    let message = Message::new(
        &vote_instruction::create_account_with_config(
            &mint_pubkey,
            &vote_pubkey,
            &VoteInit {
                node_pubkey: identity_pubkey,
                authorized_voter: vote_pubkey,
                authorized_withdrawer: vote_pubkey,
                commission: 50,
            },
            vote_balance,
            vote_instruction::CreateVoteAccountConfig {
                space: VoteStateVersions::vote_state_size_of(true) as u64,
                ..vote_instruction::CreateVoteAccountConfig::default()
            },
        ),
        Some(&mint_pubkey),
    );
    bank_client
        .send_and_confirm_message(&[&mint_keypair, &vote_keypair, &identity_keypair], message)
        .expect("failed to create vote account");

    // Create a delegated stake (NOT passive)
    let stake_rent_exempt_reserve = rent.minimum_balance(StakeStateV2::size_of());
    let stake_minimum_delegation =
        solana_stake_program::get_minimum_delegation(&bank.feature_set);
    let stake_lamports = stake_minimum_delegation + stake_rent_exempt_reserve;

    let authorized = Authorized::auto(&stake_pubkey);
    let message = Message::new(
        &stake_instruction::create_account_and_delegate_stake(
            &mint_pubkey,
            &stake_pubkey,
            &vote_pubkey,
            &authorized,
            &Lockup::default(),
            stake_lamports,
        ),
        Some(&mint_pubkey),
    );
    bank_client
        .send_and_confirm_message(&[&mint_keypair, &stake_keypair], message)
        .expect("failed to create delegated stake");

    // Verify it is actually delegated (not passive)
    let account = bank.get_account(&stake_pubkey).expect("account not found");
    let state: StakeStateV2 = account.state().expect("couldn't unpack account data");
    if let StakeStateV2::Stake(_meta, stake, flags) = state {
        assert_ne!(stake.delegation.voter_pubkey, Pubkey::default());
        assert_eq!(flags.passive_tier(), 0);
    } else {
        panic!("expected StakeStateV2::Stake for delegated, got {:?}", state);
    }

    // Send EarlyUnlock on a delegated stake — should FAIL
    let message = Message::new(
        &[pd_stake_instruction::early_unlock(
            &stake_pubkey,
            &stake_pubkey,
            &DEFAULT_TREASURY_PUBKEY,
        )],
        Some(&mint_pubkey),
    );
    assert!(
        bank_client
            .send_and_confirm_message(&[&mint_keypair, &stake_keypair], message)
            .is_err(),
        "early unlock should reject non-passive (delegated) stake"
    );
}
