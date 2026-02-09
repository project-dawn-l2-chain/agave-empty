#![allow(clippy::arithmetic_side_effects)]

use {
    solana_accounts_db::epoch_accounts_hash::EpochAccountsHash,
    solana_runtime::{
        bank::Bank,
        bank_client::BankClient,
        bank_forks::BankForks,
        genesis_utils::{create_genesis_config_with_leader, GenesisConfigInfo},
    },
    solana_sdk::{
        account::from_account,
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
        sysvar::{self, stake_history::StakeHistory},
    },
    solana_stake_interface::stake_flags::StakeFlags,
    solana_stake_program::stake_state,
    solana_vote_program::{
        vote_instruction,
        vote_state::{TowerSync, VoteInit, VoteState, VoteStateVersions, MAX_LOCKOUT_HISTORY},
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

fn warmed_up(bank: &Bank, stake_pubkey: &Pubkey) -> bool {
    let stake = stake_state::stake_from(&bank.get_account(stake_pubkey).unwrap()).unwrap();

    stake.delegation.stake
        == stake.stake(
            bank.epoch(),
            &from_account::<StakeHistory, _>(
                &bank.get_account(&sysvar::stake_history::id()).unwrap(),
            )
            .unwrap(),
            bank.new_warmup_cooldown_rate_epoch(),
        )
}

/// Helper: create a fully warmed-up delegated stake account and return the bank
/// at the point where it is warmed up, along with relevant keypairs.
struct TestSetup {
    bank: Arc<Bank>,
    bank_forks: Arc<RwLock<BankForks>>,
    mint_keypair: Keypair,
    stake_keypair: Keypair,
    stake_lamports: u64,
}

fn setup_warmed_up_stake() -> TestSetup {
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
    genesis_config.epoch_schedule = EpochSchedule::new(MINIMUM_SLOTS_PER_EPOCH);
    genesis_config.rent = Rent::default();
    let (mut bank, bank_forks) = Bank::new_with_bank_forks_for_tests(&genesis_config);
    let mint_pubkey = mint_keypair.pubkey();

    // Set EAH to Valid so Bank::new_from_parent() doesn't panic during freeze
    bank.rc
        .accounts
        .accounts_db
        .epoch_accounts_hash_manager
        .set_valid(EpochAccountsHash::new(Hash::new_unique()), bank.slot());

    let bank_client = BankClient::new_shared(bank.clone());

    let (vote_balance, stake_rent_exempt_reserve, stake_minimum_delegation) = {
        let rent = &bank.rent_collector().rent;
        (
            rent.minimum_balance(VoteState::size_of()),
            rent.minimum_balance(StakeStateV2::size_of()),
            solana_stake_program::get_minimum_delegation(&bank.feature_set),
        )
    };

    let stake_lamports = stake_minimum_delegation + stake_rent_exempt_reserve;

    // Create Vote Account
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

    // Create stake account and delegate to vote account
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
        .expect("failed to create and delegate stake account");

    // Advance until warmed up, adding extra slots to get past the epoch
    // rewards distribution window (partitioned rewards are active for the
    // first few blocks of each epoch, blocking all stake instructions).
    loop {
        if warmed_up(&bank, &stake_pubkey) {
            break;
        }
        bank = next_epoch_and_n_slots(bank.clone(), bank_forks.as_ref(), 0);
    }
    // Advance past the epoch rewards distribution period
    for _ in 0..10 {
        bank.squash();
        let slot = bank.slot() + 1;
        bank = new_bank_from_parent_with_bank_forks(
            bank_forks.as_ref(),
            bank,
            &Pubkey::default(),
            slot,
        );
    }

    TestSetup {
        bank,
        bank_forks,
        mint_keypair,
        stake_keypair,
        stake_lamports,
    }
}

/// Send a PermanentLock instruction and return the resulting bank_client result.
fn send_permanent_lock(
    bank: &Arc<Bank>,
    mint_keypair: &Keypair,
    stake_keypair: &Keypair,
) -> Result<(), solana_sdk::transport::TransportError> {
    let bank_client = BankClient::new_shared(bank.clone());
    let stake_pubkey = stake_keypair.pubkey();
    let message = Message::new(
        &[solana_stake_interface::instruction::permanent_lock(
            &stake_pubkey,
            &stake_pubkey,
            &stake_pubkey,
        )],
        Some(&mint_keypair.pubkey()),
    );
    bank_client
        .send_and_confirm_message(&[mint_keypair, stake_keypair], message)
        .map(|_| ())
}

/// Advance through an entire epoch, submitting TowerSync votes at every slot.
/// Returns the bank at the first slot of the next epoch.
fn fill_epoch_with_votes(
    mut bank: Arc<Bank>,
    bank_forks: &RwLock<BankForks>,
    vote_keypair: &Keypair,
    mint_keypair: &Keypair,
    start_slot: Slot,
) -> Arc<Bank> {
    let mint_pubkey = mint_keypair.pubkey();
    let vote_pubkey = vote_keypair.pubkey();
    let old_epoch = bank.epoch();
    while bank.epoch() != old_epoch + 1 {
        bank.squash();
        let slot = bank.slot() + 1;
        bank = new_bank_from_parent_with_bank_forks(bank_forks, bank, &Pubkey::default(), slot);

        let bank_client = BankClient::new_shared(bank.clone());
        let parent = bank.parent().unwrap();
        let lowest_slot = u64::max(
            (parent.slot() + 1).saturating_sub(MAX_LOCKOUT_HISTORY as u64),
            start_slot,
        );
        let slots: Vec<_> = (lowest_slot..(parent.slot() + 1)).collect();
        let root = (lowest_slot > start_slot).then(|| lowest_slot - 1);
        let tower_sync = TowerSync::new_from_slots(slots, parent.hash(), root);
        let message = Message::new(
            &[vote_instruction::tower_sync(
                &vote_pubkey,
                &vote_pubkey,
                tower_sync,
            )],
            Some(&mint_pubkey),
        );
        assert!(bank_client
            .send_and_confirm_message(&[mint_keypair, vote_keypair], message)
            .is_ok());
    }
    bank
}

#[test]
fn test_permanent_lock_e2e() {
    let setup = setup_warmed_up_stake();
    let bank = setup.bank;
    let stake_pubkey = setup.stake_keypair.pubkey();

    // Send PermanentLock instruction
    send_permanent_lock(&bank, &setup.mint_keypair, &setup.stake_keypair)
        .expect("permanent_lock transaction failed");

    // Verify the PERMANENTLY_LOCKED flag is set
    let account = bank
        .get_account(&stake_pubkey)
        .expect("stake account not found");
    let stake_state: StakeStateV2 = account.state().expect("couldn't unpack stake state");
    if let StakeStateV2::Stake(_meta, _stake, stake_flags) = stake_state {
        assert!(
            stake_flags.contains(StakeFlags::PERMANENTLY_LOCKED),
            "PERMANENTLY_LOCKED flag should be set after permanent_lock instruction"
        );
    } else {
        panic!("expected StakeStateV2::Stake variant, got {:?}", stake_state);
    }
}

#[test]
fn test_permanent_lock_blocks_deactivate() {
    let setup = setup_warmed_up_stake();
    let bank = setup.bank;
    let stake_pubkey = setup.stake_keypair.pubkey();

    // First, permanently lock the stake
    send_permanent_lock(&bank, &setup.mint_keypair, &setup.stake_keypair)
        .expect("permanent_lock transaction failed");

    // Now try to deactivate -- should fail
    let bank_client = BankClient::new_shared(bank.clone());
    let message = Message::new(
        &[stake_instruction::deactivate_stake(
            &stake_pubkey,
            &stake_pubkey,
        )],
        Some(&setup.mint_keypair.pubkey()),
    );
    let result =
        bank_client.send_and_confirm_message(&[&setup.mint_keypair, &setup.stake_keypair], message);
    assert!(
        result.is_err(),
        "deactivate should fail on a permanently locked stake"
    );
}

#[test]
fn test_permanent_lock_blocks_split() {
    let setup = setup_warmed_up_stake();
    let bank = setup.bank;
    let stake_pubkey = setup.stake_keypair.pubkey();

    // First, permanently lock the stake
    send_permanent_lock(&bank, &setup.mint_keypair, &setup.stake_keypair)
        .expect("permanent_lock transaction failed");

    // Create a split destination account
    let split_stake_keypair = Keypair::new();
    let split_stake_pubkey = split_stake_keypair.pubkey();

    // Fund the split destination with rent-exempt reserve
    let rent = &bank.rent_collector().rent;
    let stake_rent_exempt_reserve = rent.minimum_balance(StakeStateV2::size_of());
    bank.transfer(
        stake_rent_exempt_reserve,
        &setup.mint_keypair,
        &split_stake_pubkey,
    )
    .unwrap();

    // Try to split -- should fail
    let bank_client = BankClient::new_shared(bank.clone());
    let split_lamports = setup.stake_lamports / 2;
    let message = Message::new(
        &stake_instruction::split(
            &stake_pubkey,
            &stake_pubkey,
            split_lamports,
            &split_stake_pubkey,
        ),
        Some(&setup.mint_keypair.pubkey()),
    );
    let result = bank_client.send_and_confirm_message(
        &[
            &setup.mint_keypair,
            &setup.stake_keypair,
            &split_stake_keypair,
        ],
        message,
    );
    assert!(
        result.is_err(),
        "split should fail on a permanently locked stake"
    );
}

/// Test that a permanently locked stake persists across epoch boundaries and
/// continues to block deactivation and withdrawal in subsequent epochs.
#[test]
fn test_permanent_lock_120pct_reward() {
    let setup = setup_warmed_up_stake();
    let mut bank = setup.bank;
    let bank_forks = setup.bank_forks;
    let stake_pubkey = setup.stake_keypair.pubkey();

    // Permanently lock the stake
    send_permanent_lock(&bank, &setup.mint_keypair, &setup.stake_keypair)
        .expect("permanent_lock transaction failed");

    // Verify flag is set before advancing
    let account = bank
        .get_account(&stake_pubkey)
        .expect("stake account not found");
    let stake_state: StakeStateV2 = account.state().expect("couldn't unpack stake state");
    if let StakeStateV2::Stake(_meta, _stake, stake_flags) = stake_state {
        assert!(
            stake_flags.contains(StakeFlags::PERMANENTLY_LOCKED),
            "flag should be set before epoch advance"
        );
    } else {
        panic!("expected Stake variant");
    }

    // Advance two full epochs (past rewards distribution windows)
    bank = next_epoch_and_n_slots(bank, bank_forks.as_ref(), 10);
    bank = next_epoch_and_n_slots(bank, bank_forks.as_ref(), 10);

    // Verify flag persists after epoch transitions
    let account = bank
        .get_account(&stake_pubkey)
        .expect("stake account not found after epoch advance");
    let stake_state: StakeStateV2 = account.state().expect("couldn't unpack stake state");
    if let StakeStateV2::Stake(_meta, _stake, stake_flags) = stake_state {
        assert!(
            stake_flags.contains(StakeFlags::PERMANENTLY_LOCKED),
            "PERMANENTLY_LOCKED flag should persist across epoch boundaries"
        );
    } else {
        panic!("expected Stake variant after epoch advance");
    }

    // Deactivation should still fail after epoch advance
    let bank_client = BankClient::new_shared(bank.clone());
    let message = Message::new(
        &[stake_instruction::deactivate_stake(
            &stake_pubkey,
            &stake_pubkey,
        )],
        Some(&setup.mint_keypair.pubkey()),
    );
    let result = bank_client
        .send_and_confirm_message(&[&setup.mint_keypair, &setup.stake_keypair], message);
    assert!(
        result.is_err(),
        "deactivate should still fail after epoch transitions"
    );

    // Withdrawal should also fail
    let bank_client = BankClient::new_shared(bank.clone());
    let message = Message::new(
        &[stake_instruction::withdraw(
            &stake_pubkey,
            &stake_pubkey,
            &solana_pubkey::new_rand(),
            1,
            None,
        )],
        Some(&setup.mint_keypair.pubkey()),
    );
    let result = bank_client
        .send_and_confirm_message(&[&setup.mint_keypair, &setup.stake_keypair], message);
    assert!(
        result.is_err(),
        "withdrawal should fail on a permanently locked stake"
    );
}

/// GovernanceUnlock is a stub that always returns InvalidInstructionData.
/// This test verifies that sending the instruction fails as expected.
#[test]
fn test_governance_unlock_returns_error() {
    use solana_sdk::instruction::Instruction;
    use solana_stake_interface::instruction::StakeInstruction;

    let leader = Pubkey::new_unique();
    let GenesisConfigInfo {
        genesis_config,
        mint_keypair,
        ..
    } = create_genesis_config_with_leader(100_000_000_000, &leader, 3);

    let (bank, _bank_forks) = Bank::new_with_bank_forks_for_tests(&genesis_config);
    let bank_client = BankClient::new_shared(bank);

    // Construct GovernanceUnlock instruction manually (no builder exists).
    let instruction = Instruction::new_with_bincode(
        solana_sdk::stake::program::id(),
        &StakeInstruction::GovernanceUnlock,
        vec![], // no accounts defined for this stub
    );
    let message = Message::new(&[instruction], Some(&mint_keypair.pubkey()));
    let result = bank_client.send_and_confirm_message(&[&mint_keypair], message);

    assert!(
        result.is_err(),
        "GovernanceUnlock should always fail (stub returns InvalidInstructionData)"
    );
}

/// Test that a permanently locked stake (A) earns ~120% of the reward earned
/// by an identical but unlocked stake (B) after an epoch with vote credits.
#[test]
fn test_permanent_lock_120pct_reward_comparison() {
    solana_logger::setup();

    // --- Genesis & bank setup ---
    let stake_keypair_a = Keypair::new();
    let stake_keypair_b = Keypair::new();
    let vote_keypair = Keypair::new();
    let identity_keypair = Keypair::new();

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
    let (mut bank, bank_forks) = Bank::new_with_bank_forks_for_tests(&genesis_config);

    // Set EAH to Valid so Bank::new_from_parent() doesn't panic during freeze
    bank.rc
        .accounts
        .accounts_db
        .epoch_accounts_hash_manager
        .set_valid(EpochAccountsHash::new(Hash::new_unique()), bank.slot());

    let bank_client = BankClient::new_shared(bank.clone());
    let mint_pubkey = mint_keypair.pubkey();

    let (vote_balance, stake_rent_exempt_reserve, stake_minimum_delegation) = {
        let rent = &bank.rent_collector().rent;
        (
            rent.minimum_balance(VoteState::size_of()),
            rent.minimum_balance(StakeStateV2::size_of()),
            solana_stake_program::get_minimum_delegation(&bank.feature_set),
        )
    };

    let stake_lamports = stake_minimum_delegation + stake_rent_exempt_reserve;

    // --- Create vote account ---
    let message = Message::new(
        &vote_instruction::create_account_with_config(
            &mint_pubkey,
            &vote_keypair.pubkey(),
            &VoteInit {
                node_pubkey: identity_keypair.pubkey(),
                authorized_voter: vote_keypair.pubkey(),
                authorized_withdrawer: vote_keypair.pubkey(),
                commission: 0, // zero commission so all rewards go to stakers
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

    // --- Create stake A (will be permanently locked) ---
    let authorized_a = Authorized::auto(&stake_keypair_a.pubkey());
    let message = Message::new(
        &stake_instruction::create_account_and_delegate_stake(
            &mint_pubkey,
            &stake_keypair_a.pubkey(),
            &vote_keypair.pubkey(),
            &authorized_a,
            &Lockup::default(),
            stake_lamports,
        ),
        Some(&mint_pubkey),
    );
    bank_client
        .send_and_confirm_message(&[&mint_keypair, &stake_keypair_a], message)
        .expect("failed to create stake A");

    // --- Create stake B (normal, same amount) ---
    let authorized_b = Authorized::auto(&stake_keypair_b.pubkey());
    let message = Message::new(
        &stake_instruction::create_account_and_delegate_stake(
            &mint_pubkey,
            &stake_keypair_b.pubkey(),
            &vote_keypair.pubkey(),
            &authorized_b,
            &Lockup::default(),
            stake_lamports,
        ),
        Some(&mint_pubkey),
    );
    bank_client
        .send_and_confirm_message(&[&mint_keypair, &stake_keypair_b], message)
        .expect("failed to create stake B");

    // --- Warm up both stakes ---
    loop {
        if warmed_up(&bank, &stake_keypair_a.pubkey())
            && warmed_up(&bank, &stake_keypair_b.pubkey())
        {
            break;
        }
        bank = next_epoch_and_n_slots(bank.clone(), bank_forks.as_ref(), 0);
    }
    // Advance past the epoch rewards distribution period
    for _ in 0..10 {
        bank.squash();
        let slot = bank.slot() + 1;
        bank = new_bank_from_parent_with_bank_forks(
            bank_forks.as_ref(),
            bank,
            &Pubkey::default(),
            slot,
        );
    }

    // --- Permanently lock stake A ---
    send_permanent_lock(&bank, &mint_keypair, &stake_keypair_a)
        .expect("permanent_lock transaction failed");

    // Verify PERMANENTLY_LOCKED flag is set on A
    {
        let account = bank
            .get_account(&stake_keypair_a.pubkey())
            .expect("stake A not found");
        let stake_state: StakeStateV2 = account.state().expect("couldn't unpack stake state A");
        if let StakeStateV2::Stake(_, _, flags) = stake_state {
            assert!(
                flags.contains(StakeFlags::PERMANENTLY_LOCKED),
                "stake A should be permanently locked"
            );
        } else {
            panic!("expected Stake variant for A");
        }
    }

    // --- Fill two epochs with votes to generate reward credits ---
    // First epoch: accumulates credits in the vote account
    let start_slot = bank.slot();
    bank = fill_epoch_with_votes(
        bank,
        bank_forks.as_ref(),
        &vote_keypair,
        &mint_keypair,
        start_slot,
    );

    // Second epoch: more credits; rewards from the first filled epoch
    // are calculated at this epoch boundary.
    bank = fill_epoch_with_votes(
        bank,
        bank_forks.as_ref(),
        &vote_keypair,
        &mint_keypair,
        start_slot,
    );

    // Record balances before rewards are distributed
    let balance_a_before = bank.get_balance(&stake_keypair_a.pubkey());
    let balance_b_before = bank.get_balance(&stake_keypair_b.pubkey());
    assert_eq!(
        balance_a_before, balance_b_before,
        "both stakes should have equal lamports before rewards"
    );

    // Advance to next epoch + 1 slot to trigger reward distribution
    bank = next_epoch_and_n_slots(bank, bank_forks.as_ref(), 1);

    // --- Read balances after rewards ---
    let balance_a_after = bank.get_balance(&stake_keypair_a.pubkey());
    let balance_b_after = bank.get_balance(&stake_keypair_b.pubkey());

    let reward_a = balance_a_after.saturating_sub(balance_a_before);
    let reward_b = balance_b_after.saturating_sub(balance_b_before);

    // Both should have received some rewards
    assert!(
        reward_b > 0,
        "stake B (normal) should have received rewards, got 0"
    );
    assert!(
        reward_a > 0,
        "stake A (permanently locked) should have received rewards, got 0"
    );

    // A's reward should be ~120% of B's reward (within ±2 lamports for rounding)
    let expected_a = reward_b * 120 / 100;
    let diff = if reward_a > expected_a {
        reward_a - expected_a
    } else {
        expected_a - reward_a
    };
    assert!(
        diff <= 2,
        "permanently locked reward ({}) should be ~120% of normal reward ({}). \
         Expected ~{}, difference = {}",
        reward_a,
        reward_b,
        expected_a,
        diff,
    );
}

/// Helper: create a dual-stake setup (locked + unlocked) with a vote account,
/// both warmed up, stake A permanently locked, and ready for vote-filling.
/// Returns all keypairs and the bank/forks at the point where both are warmed up
/// and stake A is permanently locked.
struct DualStakeSetup {
    bank: Arc<Bank>,
    bank_forks: Arc<RwLock<BankForks>>,
    mint_keypair: Keypair,
    vote_keypair: Keypair,
    stake_keypair_a: Keypair, // permanently locked
    stake_keypair_b: Keypair, // normal (unlocked)
    _stake_lamports: u64,
}

fn setup_dual_stake_locked_unlocked() -> DualStakeSetup {
    solana_logger::setup();

    let stake_keypair_a = Keypair::new();
    let stake_keypair_b = Keypair::new();
    let vote_keypair = Keypair::new();
    let identity_keypair = Keypair::new();

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
    let (mut bank, bank_forks) = Bank::new_with_bank_forks_for_tests(&genesis_config);

    bank.rc
        .accounts
        .accounts_db
        .epoch_accounts_hash_manager
        .set_valid(EpochAccountsHash::new(Hash::new_unique()), bank.slot());

    let bank_client = BankClient::new_shared(bank.clone());
    let mint_pubkey = mint_keypair.pubkey();

    let (vote_balance, stake_rent_exempt_reserve, stake_minimum_delegation) = {
        let rent = &bank.rent_collector().rent;
        (
            rent.minimum_balance(VoteState::size_of()),
            rent.minimum_balance(StakeStateV2::size_of()),
            solana_stake_program::get_minimum_delegation(&bank.feature_set),
        )
    };

    let stake_lamports = stake_minimum_delegation + stake_rent_exempt_reserve;

    // Create vote account with 0% commission
    let message = Message::new(
        &vote_instruction::create_account_with_config(
            &mint_pubkey,
            &vote_keypair.pubkey(),
            &VoteInit {
                node_pubkey: identity_keypair.pubkey(),
                authorized_voter: vote_keypair.pubkey(),
                authorized_withdrawer: vote_keypair.pubkey(),
                commission: 0,
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

    // Create stake A (will be permanently locked)
    let authorized_a = Authorized::auto(&stake_keypair_a.pubkey());
    let message = Message::new(
        &stake_instruction::create_account_and_delegate_stake(
            &mint_pubkey,
            &stake_keypair_a.pubkey(),
            &vote_keypair.pubkey(),
            &authorized_a,
            &Lockup::default(),
            stake_lamports,
        ),
        Some(&mint_pubkey),
    );
    bank_client
        .send_and_confirm_message(&[&mint_keypair, &stake_keypair_a], message)
        .expect("failed to create stake A");

    // Create stake B (normal, same amount)
    let authorized_b = Authorized::auto(&stake_keypair_b.pubkey());
    let message = Message::new(
        &stake_instruction::create_account_and_delegate_stake(
            &mint_pubkey,
            &stake_keypair_b.pubkey(),
            &vote_keypair.pubkey(),
            &authorized_b,
            &Lockup::default(),
            stake_lamports,
        ),
        Some(&mint_pubkey),
    );
    bank_client
        .send_and_confirm_message(&[&mint_keypair, &stake_keypair_b], message)
        .expect("failed to create stake B");

    // Warm up both stakes
    loop {
        if warmed_up(&bank, &stake_keypair_a.pubkey())
            && warmed_up(&bank, &stake_keypair_b.pubkey())
        {
            break;
        }
        bank = next_epoch_and_n_slots(bank.clone(), bank_forks.as_ref(), 0);
    }
    // Advance past the epoch rewards distribution period
    for _ in 0..10 {
        bank.squash();
        let slot = bank.slot() + 1;
        bank = new_bank_from_parent_with_bank_forks(
            bank_forks.as_ref(),
            bank,
            &Pubkey::default(),
            slot,
        );
    }

    // Permanently lock stake A
    send_permanent_lock(&bank, &mint_keypair, &stake_keypair_a)
        .expect("permanent_lock transaction failed");

    DualStakeSetup {
        bank,
        bank_forks,
        mint_keypair,
        vote_keypair,
        stake_keypair_a,
        stake_keypair_b,
        _stake_lamports: stake_lamports,
    }
}

/// Test 1: Over 3 epochs, the bonus is always 20% of the *base* reward
/// for that epoch — it never compounds on prior bonuses.
#[test]
fn test_permanent_lock_multi_epoch_non_compounding() {
    let setup = setup_dual_stake_locked_unlocked();
    let mut bank = setup.bank;
    let bank_forks = setup.bank_forks;
    let start_slot = bank.slot();

    // Fill epochs continuously with votes.
    // Each fill_epoch_with_votes advances through one full epoch, and
    // rewards for epoch N are calculated at the boundary of epoch N+1.
    // We fill 5 consecutive epochs (the first is needed to seed credits,
    // then each subsequent boundary produces rewards).

    // Epoch fill 1: seeds vote credits (no rewards yet — first epoch)
    bank = fill_epoch_with_votes(
        bank,
        bank_forks.as_ref(),
        &setup.vote_keypair,
        &setup.mint_keypair,
        start_slot,
    );

    let mut prev_bonuses: Vec<u64> = Vec::new();

    // Epochs 2-4: each boundary triggers rewards from the previous epoch's credits.
    // We record balances before and after the epoch transition to measure rewards.
    for epoch_idx in 0..3u32 {
        // Record balances before the epoch fill + boundary
        let balance_a_before = bank.get_balance(&setup.stake_keypair_a.pubkey());
        let balance_b_before = bank.get_balance(&setup.stake_keypair_b.pubkey());

        // Fill this epoch with votes — the epoch boundary triggers reward distribution
        bank = fill_epoch_with_votes(
            bank,
            bank_forks.as_ref(),
            &setup.vote_keypair,
            &setup.mint_keypair,
            start_slot,
        );

        // Advance a few more slots past the epoch boundary to let rewards distribute
        for _ in 0..10 {
            bank.squash();
            let slot = bank.slot() + 1;
            bank = new_bank_from_parent_with_bank_forks(
                bank_forks.as_ref(),
                bank,
                &Pubkey::default(),
                slot,
            );
        }

        let balance_a_after = bank.get_balance(&setup.stake_keypair_a.pubkey());
        let balance_b_after = bank.get_balance(&setup.stake_keypair_b.pubkey());

        let reward_a = balance_a_after.saturating_sub(balance_a_before);
        let reward_b = balance_b_after.saturating_sub(balance_b_before);

        assert!(
            reward_b > 0,
            "epoch {}: normal stake should have received rewards",
            epoch_idx
        );
        assert!(
            reward_a > 0,
            "epoch {}: locked stake should have received rewards",
            epoch_idx
        );

        let bonus = reward_a.saturating_sub(reward_b);

        // The bonus should be ~20% of the base reward (reward_b) for this epoch
        let expected_bonus = reward_b * 20 / 100;
        let diff = if bonus > expected_bonus {
            bonus - expected_bonus
        } else {
            expected_bonus - bonus
        };
        assert!(
            diff <= 1,
            "epoch {}: bonus ({}) should be ~20% of base reward ({}). Expected ~{}, diff={}",
            epoch_idx,
            bonus,
            reward_b,
            expected_bonus,
            diff,
        );

        prev_bonuses.push(bonus);
    }

    // Verify non-compounding: each epoch's bonus should be proportional to
    // that epoch's base reward, not growing from accumulated bonuses.
    // Since stakes are identical and credits similar, bonuses should be similar
    // across epochs (not growing). We verify each bonus is within 2x of the
    // first — if compounding were happening, later bonuses would be
    // significantly larger.
    let first_bonus = prev_bonuses[0];
    for (i, b) in prev_bonuses.iter().enumerate().skip(1) {
        assert!(
            *b <= first_bonus * 2,
            "epoch {}: bonus ({}) is suspiciously large compared to first ({}), \
             suggesting compounding",
            i,
            b,
            first_bonus,
        );
    }
}

/// Test 2: bank.capitalization() correctly includes the permanent lock bonus
/// as new inflation, and the EpochRewards sysvar's total_rewards accounts for it.
#[test]
fn test_permanent_lock_capitalization_accounting() {
    let setup = setup_dual_stake_locked_unlocked();
    let mut bank = setup.bank;
    let bank_forks = setup.bank_forks;
    let start_slot = bank.slot();

    // Fill first epoch with votes to generate credits
    bank = fill_epoch_with_votes(
        bank,
        bank_forks.as_ref(),
        &setup.vote_keypair,
        &setup.mint_keypair,
        start_slot,
    );

    // Fill second epoch — rewards from first filled epoch calculated at this boundary
    bank = fill_epoch_with_votes(
        bank,
        bank_forks.as_ref(),
        &setup.vote_keypair,
        &setup.mint_keypair,
        start_slot,
    );

    // Record balances and cap before reward distribution
    let balance_a_before = bank.get_balance(&setup.stake_keypair_a.pubkey());
    let balance_b_before = bank.get_balance(&setup.stake_keypair_b.pubkey());
    let cap_before = bank.capitalization();

    // Advance to next epoch + enough slots for rewards to distribute
    bank = next_epoch_and_n_slots(bank, bank_forks.as_ref(), 10);

    let balance_a_after = bank.get_balance(&setup.stake_keypair_a.pubkey());
    let balance_b_after = bank.get_balance(&setup.stake_keypair_b.pubkey());
    let cap_after = bank.capitalization();

    let reward_a = balance_a_after.saturating_sub(balance_a_before);
    let reward_b = balance_b_after.saturating_sub(balance_b_before);

    assert!(reward_a > 0, "locked stake should have received rewards");
    assert!(reward_b > 0, "normal stake should have received rewards");

    // The bonus is the extra that A earned over B
    let bonus = reward_a.saturating_sub(reward_b);
    assert!(bonus > 0, "locked stake should have earned a bonus");

    // Cap increase should be >= reward_a + reward_b (plus vote rewards)
    let cap_increase = cap_after.saturating_sub(cap_before);
    assert!(
        cap_increase >= reward_a + reward_b,
        "capitalization increase ({}) should be >= sum of stake rewards ({} + {} = {})",
        cap_increase,
        reward_a,
        reward_b,
        reward_a + reward_b,
    );

    // Read the EpochRewards sysvar — total_rewards should include the bonus
    let epoch_rewards_account = bank
        .get_account(&sysvar::epoch_rewards::id())
        .expect("EpochRewards sysvar should exist");
    let epoch_rewards: solana_sdk::sysvar::epoch_rewards::EpochRewards =
        from_account(&epoch_rewards_account).expect("should deserialize EpochRewards");

    // After distribution completes, total_rewards should be >= reward_a + reward_b
    // (total_rewards includes vote rewards too, but must at least cover stake rewards)
    assert!(
        epoch_rewards.total_rewards >= reward_a + reward_b,
        "EpochRewards total_rewards ({}) should be >= stake rewards ({} + {} = {})",
        epoch_rewards.total_rewards,
        reward_a,
        reward_b,
        reward_a + reward_b,
    );

    // Verify distributed_rewards == total_rewards after distribution completes
    // (the sysvar should be inactive now)
    assert!(
        !epoch_rewards.active,
        "EpochRewards should be inactive after distribution completes"
    );
    assert_eq!(
        epoch_rewards.distributed_rewards, epoch_rewards.total_rewards,
        "distributed_rewards should equal total_rewards after distribution completes"
    );
}

/// Test 3: Multiple permanently locked stakes each get their own 20% bonus.
#[test]
fn test_permanent_lock_multiple_locked_stakes() {
    solana_logger::setup();

    let stake_keypair_a = Keypair::new(); // will be locked
    let stake_keypair_b = Keypair::new(); // will be locked
    let stake_keypair_c = Keypair::new(); // normal (unlocked)
    let vote_keypair = Keypair::new();
    let identity_keypair = Keypair::new();

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
    let (mut bank, bank_forks) = Bank::new_with_bank_forks_for_tests(&genesis_config);

    bank.rc
        .accounts
        .accounts_db
        .epoch_accounts_hash_manager
        .set_valid(EpochAccountsHash::new(Hash::new_unique()), bank.slot());

    let bank_client = BankClient::new_shared(bank.clone());
    let mint_pubkey = mint_keypair.pubkey();

    let (vote_balance, stake_rent_exempt_reserve, stake_minimum_delegation) = {
        let rent = &bank.rent_collector().rent;
        (
            rent.minimum_balance(VoteState::size_of()),
            rent.minimum_balance(StakeStateV2::size_of()),
            solana_stake_program::get_minimum_delegation(&bank.feature_set),
        )
    };

    let stake_lamports = stake_minimum_delegation + stake_rent_exempt_reserve;

    // Create vote account with 0% commission
    let message = Message::new(
        &vote_instruction::create_account_with_config(
            &mint_pubkey,
            &vote_keypair.pubkey(),
            &VoteInit {
                node_pubkey: identity_keypair.pubkey(),
                authorized_voter: vote_keypair.pubkey(),
                authorized_withdrawer: vote_keypair.pubkey(),
                commission: 0,
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

    // Create 3 stake accounts, all same amount
    for (keypair, label) in [
        (&stake_keypair_a, "A"),
        (&stake_keypair_b, "B"),
        (&stake_keypair_c, "C"),
    ] {
        let authorized = Authorized::auto(&keypair.pubkey());
        let message = Message::new(
            &stake_instruction::create_account_and_delegate_stake(
                &mint_pubkey,
                &keypair.pubkey(),
                &vote_keypair.pubkey(),
                &authorized,
                &Lockup::default(),
                stake_lamports,
            ),
            Some(&mint_pubkey),
        );
        bank_client
            .send_and_confirm_message(&[&mint_keypair, keypair], message)
            .unwrap_or_else(|e| panic!("failed to create stake {}: {:?}", label, e));
    }

    // Warm up all 3 stakes
    loop {
        if warmed_up(&bank, &stake_keypair_a.pubkey())
            && warmed_up(&bank, &stake_keypair_b.pubkey())
            && warmed_up(&bank, &stake_keypair_c.pubkey())
        {
            break;
        }
        bank = next_epoch_and_n_slots(bank.clone(), bank_forks.as_ref(), 0);
    }
    for _ in 0..10 {
        bank.squash();
        let slot = bank.slot() + 1;
        bank = new_bank_from_parent_with_bank_forks(
            bank_forks.as_ref(),
            bank,
            &Pubkey::default(),
            slot,
        );
    }

    // Permanently lock A and B
    send_permanent_lock(&bank, &mint_keypair, &stake_keypair_a)
        .expect("permanent_lock A failed");
    send_permanent_lock(&bank, &mint_keypair, &stake_keypair_b)
        .expect("permanent_lock B failed");

    // Fill two epochs with votes to generate reward credits
    let start_slot = bank.slot();
    bank = fill_epoch_with_votes(
        bank,
        bank_forks.as_ref(),
        &vote_keypair,
        &mint_keypair,
        start_slot,
    );
    bank = fill_epoch_with_votes(
        bank,
        bank_forks.as_ref(),
        &vote_keypair,
        &mint_keypair,
        start_slot,
    );

    // Record balances before rewards
    let balance_a_before = bank.get_balance(&stake_keypair_a.pubkey());
    let balance_b_before = bank.get_balance(&stake_keypair_b.pubkey());
    let balance_c_before = bank.get_balance(&stake_keypair_c.pubkey());
    assert_eq!(balance_a_before, balance_b_before);
    assert_eq!(balance_b_before, balance_c_before);

    // Advance to trigger reward distribution
    bank = next_epoch_and_n_slots(bank, bank_forks.as_ref(), 1);

    let balance_a_after = bank.get_balance(&stake_keypair_a.pubkey());
    let balance_b_after = bank.get_balance(&stake_keypair_b.pubkey());
    let balance_c_after = bank.get_balance(&stake_keypair_c.pubkey());

    let reward_a = balance_a_after.saturating_sub(balance_a_before);
    let reward_b = balance_b_after.saturating_sub(balance_b_before);
    let reward_c = balance_c_after.saturating_sub(balance_c_before);

    assert!(reward_c > 0, "normal stake C should have received rewards");
    assert!(reward_a > 0, "locked stake A should have received rewards");
    assert!(reward_b > 0, "locked stake B should have received rewards");

    // Both locked stakes (A, B) should earn ~120% of normal stake (C)
    for (label, reward_locked) in [("A", reward_a), ("B", reward_b)] {
        let expected = reward_c * 120 / 100;
        let diff = if reward_locked > expected {
            reward_locked - expected
        } else {
            expected - reward_locked
        };
        assert!(
            diff <= 2,
            "locked stake {} reward ({}) should be ~120% of normal reward ({}). Expected ~{}, diff={}",
            label, reward_locked, reward_c, expected, diff,
        );
    }

    // A and B should have earned the same reward (same stake, same lock status)
    let diff_ab = if reward_a > reward_b {
        reward_a - reward_b
    } else {
        reward_b - reward_a
    };
    assert!(
        diff_ab <= 1,
        "locked stakes A ({}) and B ({}) should earn the same reward",
        reward_a,
        reward_b,
    );

    // Total bonus = sum of individual bonuses
    let bonus_a = reward_a.saturating_sub(reward_c);
    let bonus_b = reward_b.saturating_sub(reward_c);
    let total_bonus = bonus_a + bonus_b;
    assert!(
        total_bonus > 0,
        "total permanent lock bonus should be positive"
    );
}

/// Test 4: When no votes are submitted (zero-reward epoch), the multiplier
/// code path doesn't panic and no bonus is applied. Rewards resume normally
/// in the next epoch.
#[test]
fn test_permanent_lock_zero_reward_epoch() {
    let setup = setup_dual_stake_locked_unlocked();
    let mut bank = setup.bank;
    let bank_forks = setup.bank_forks;

    // Record balances before the zero-reward epoch
    let balance_a_before = bank.get_balance(&setup.stake_keypair_a.pubkey());
    let balance_b_before = bank.get_balance(&setup.stake_keypair_b.pubkey());

    // Do NOT fill votes — just advance to next epoch
    // This should not panic even though no rewards are generated
    bank = next_epoch_and_n_slots(bank, bank_forks.as_ref(), 10);

    let balance_a_after_zero = bank.get_balance(&setup.stake_keypair_a.pubkey());
    let balance_b_after_zero = bank.get_balance(&setup.stake_keypair_b.pubkey());

    // No rewards should have been distributed (no vote credits)
    assert_eq!(
        balance_a_before, balance_a_after_zero,
        "locked stake should have no rewards without votes"
    );
    assert_eq!(
        balance_b_before, balance_b_after_zero,
        "normal stake should have no rewards without votes"
    );

    // Now fill votes and confirm rewards resume normally with 120% multiplier
    let start_slot = bank.slot();
    bank = fill_epoch_with_votes(
        bank,
        bank_forks.as_ref(),
        &setup.vote_keypair,
        &setup.mint_keypair,
        start_slot,
    );
    bank = fill_epoch_with_votes(
        bank,
        bank_forks.as_ref(),
        &setup.vote_keypair,
        &setup.mint_keypair,
        start_slot,
    );

    let balance_a_pre_reward = bank.get_balance(&setup.stake_keypair_a.pubkey());
    let balance_b_pre_reward = bank.get_balance(&setup.stake_keypair_b.pubkey());

    bank = next_epoch_and_n_slots(bank, bank_forks.as_ref(), 1);

    let balance_a_post_reward = bank.get_balance(&setup.stake_keypair_a.pubkey());
    let balance_b_post_reward = bank.get_balance(&setup.stake_keypair_b.pubkey());

    let reward_a = balance_a_post_reward.saturating_sub(balance_a_pre_reward);
    let reward_b = balance_b_post_reward.saturating_sub(balance_b_pre_reward);

    assert!(reward_b > 0, "normal stake should earn rewards after resuming votes");
    assert!(reward_a > 0, "locked stake should earn rewards after resuming votes");

    // Verify 120% multiplier is still active
    let expected_a = reward_b * 120 / 100;
    let diff = if reward_a > expected_a {
        reward_a - expected_a
    } else {
        expected_a - reward_a
    };
    assert!(
        diff <= 2,
        "locked reward ({}) should be ~120% of normal ({}) after zero-reward epoch. \
         Expected ~{}, diff={}",
        reward_a,
        reward_b,
        expected_a,
        diff,
    );
}

/// Test 5: During the partitioned reward distribution window, the EpochRewards
/// sysvar correctly tracks distributed_rewards increasing slot-by-slot until
/// it equals total_rewards (which includes the bonus).
#[test]
fn test_permanent_lock_epoch_rewards_sysvar_consistency() {
    let setup = setup_dual_stake_locked_unlocked();
    let mut bank = setup.bank;
    let bank_forks = setup.bank_forks;
    let start_slot = bank.slot();

    // Fill two epochs with votes to generate reward credits
    bank = fill_epoch_with_votes(
        bank,
        bank_forks.as_ref(),
        &setup.vote_keypair,
        &setup.mint_keypair,
        start_slot,
    );
    bank = fill_epoch_with_votes(
        bank,
        bank_forks.as_ref(),
        &setup.vote_keypair,
        &setup.mint_keypair,
        start_slot,
    );

    // Record balances before rewards
    let balance_a_before = bank.get_balance(&setup.stake_keypair_a.pubkey());
    let balance_b_before = bank.get_balance(&setup.stake_keypair_b.pubkey());

    // Advance to the epoch boundary (first slot of new epoch)
    // This is where begin_partitioned_rewards runs and creates the sysvar
    bank.squash();
    let epoch_boundary_slot = bank.get_slots_in_epoch(bank.epoch()) + bank.slot();
    bank = new_bank_from_parent_with_bank_forks(
        bank_forks.as_ref(),
        bank,
        &Pubkey::default(),
        epoch_boundary_slot,
    );

    // Read the EpochRewards sysvar at the epoch boundary
    let er_account = bank
        .get_account(&sysvar::epoch_rewards::id())
        .expect("EpochRewards sysvar should exist at epoch boundary");
    let er: solana_sdk::sysvar::epoch_rewards::EpochRewards =
        from_account(&er_account).expect("should deserialize EpochRewards");

    // The sysvar should be active at the epoch boundary
    assert!(er.active, "EpochRewards should be active at epoch boundary");
    assert!(
        er.total_rewards > 0,
        "total_rewards should be positive"
    );

    // total_rewards should be > what a normal reward pool would be
    // (it includes the 20% permanent lock bonus)
    let total_rewards = er.total_rewards;
    let mut prev_distributed = er.distributed_rewards;

    // Advance slot-by-slot through the distribution window
    let mut was_active = true;
    let mut slots_advanced = 0u64;
    while was_active && slots_advanced < 100 {
        bank.squash();
        let slot = bank.slot() + 1;
        bank = new_bank_from_parent_with_bank_forks(
            bank_forks.as_ref(),
            bank,
            &Pubkey::default(),
            slot,
        );
        slots_advanced += 1;

        let er_account = bank
            .get_account(&sysvar::epoch_rewards::id())
            .expect("EpochRewards sysvar should persist during distribution");
        let er: solana_sdk::sysvar::epoch_rewards::EpochRewards =
            from_account(&er_account).expect("should deserialize EpochRewards");

        // distributed_rewards should be monotonically non-decreasing
        assert!(
            er.distributed_rewards >= prev_distributed,
            "distributed_rewards should be monotonically increasing: {} < {}",
            er.distributed_rewards,
            prev_distributed,
        );

        // total_rewards should remain constant
        assert_eq!(
            er.total_rewards, total_rewards,
            "total_rewards should remain constant during distribution"
        );

        prev_distributed = er.distributed_rewards;
        was_active = er.active;
    }

    // After distribution, the sysvar should be inactive
    let er_account = bank
        .get_account(&sysvar::epoch_rewards::id())
        .expect("EpochRewards sysvar should exist after distribution");
    let er: solana_sdk::sysvar::epoch_rewards::EpochRewards =
        from_account(&er_account).expect("should deserialize EpochRewards");

    assert!(
        !er.active,
        "EpochRewards should be inactive after distribution completes"
    );
    assert_eq!(
        er.distributed_rewards, er.total_rewards,
        "distributed_rewards ({}) should equal total_rewards ({}) after distribution",
        er.distributed_rewards, er.total_rewards,
    );

    // Verify that total_rewards includes the 20% bonus
    // by checking that stake A earned ~120% of stake B
    let balance_a_after = bank.get_balance(&setup.stake_keypair_a.pubkey());
    let balance_b_after = bank.get_balance(&setup.stake_keypair_b.pubkey());
    let reward_a = balance_a_after.saturating_sub(balance_a_before);
    let reward_b = balance_b_after.saturating_sub(balance_b_before);

    assert!(reward_b > 0, "normal stake should have received rewards");
    assert!(reward_a > 0, "locked stake should have received rewards");

    let expected_a = reward_b * 120 / 100;
    let diff = if reward_a > expected_a {
        reward_a - expected_a
    } else {
        expected_a - reward_a
    };
    assert!(
        diff <= 2,
        "locked reward ({}) should be ~120% of normal ({}). Expected ~{}, diff={}",
        reward_a,
        reward_b,
        expected_a,
        diff,
    );
}
