#![allow(clippy::arithmetic_side_effects)]

use {
    solana_accounts_db::accounts_db::CalcAccountsHashDataSource,
    solana_runtime::{
        bank::Bank,
        bank::test_utils::goto_end_of_slot,
        bank_forks::BankForks,
        genesis_utils::{create_genesis_config_with_leader, GenesisConfigInfo},
        projectdawn_config::{DEFAULT_TREASURY_PUBKEY, PROJECTDAWN_FEATURE_ID, ProjectDawnConfig},
    },
    solana_sdk::{
        account::{Account, AccountSharedData, ReadableAccount},
        clock::Slot,
        epoch_schedule::EpochSchedule,
        feature::{self, Feature},
        fee_calculator::FeeRateGovernor,
        pubkey::Pubkey,
        rent::Rent,
        system_program,
        system_transaction,
    },
    std::sync::{Arc, RwLock},
};

/// Helper: create a bank with ProjectDawn feature enabled via genesis.
///
/// Inserts the treasury account (rent-exempt, system-owned) and a Feature
/// account for PROJECTDAWN_FEATURE_ID into genesis so that `finish_init` /
/// `apply_feature_activations` detects and enables ProjectDawn economics.
///
/// Uses a non-zero fee rate (5000 lamports/sig) so transactions generate fees.
fn setup_projectdawn_bank() -> (Arc<Bank>, Arc<RwLock<BankForks>>, solana_sdk::signature::Keypair) {
    let leader = Pubkey::new_unique();
    let mint_lamports = 100_000_000_000; // 100 SOL
    let GenesisConfigInfo {
        mut genesis_config,
        mint_keypair,
        ..
    } = create_genesis_config_with_leader(mint_lamports, &leader, 3);

    // Non-zero fees so transactions actually generate fee revenue.
    genesis_config.fee_rate_governor = FeeRateGovernor::new(5000, 0);

    let rent = Rent::default();

    // Treasury account: system-owned, rent-exempt for 0-data account.
    let treasury_balance = rent.minimum_balance(0);
    let treasury_account = AccountSharedData::new(treasury_balance, 0, &system_program::id());
    genesis_config
        .accounts
        .insert(DEFAULT_TREASURY_PUBKEY, Account::from(treasury_account));

    // Feature account for PROJECTDAWN_FEATURE_ID (pre-activated at slot 0).
    // Using activated_at: Some(0) because FinishInit does not activate pending
    // features (only NewFromParent does). Pre-activating ensures it's recognized
    // by compute_active_feature_set during bank initialization.
    let feature_balance = std::cmp::max(rent.minimum_balance(Feature::size_of()), 1);
    let feature_account =
        feature::create_account(&Feature { activated_at: Some(0) }, feature_balance);
    genesis_config
        .accounts
        .insert(PROJECTDAWN_FEATURE_ID, Account::from(feature_account));

    let (bank, bank_forks) = Bank::new_with_bank_forks_for_tests(&genesis_config);
    (bank, bank_forks, mint_keypair)
}

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

// ---------------------------------------------------------------------------
// Test 1: 4-way fee split at launch (epoch 0)
// ---------------------------------------------------------------------------
#[test]
fn test_fee_split_4way_at_launch() {
    let (bank, _bank_forks, mint_keypair) = setup_projectdawn_bank();

    // Verify ProjectDawn is enabled.
    assert!(
        bank.projectdawn_enabled,
        "ProjectDawn feature should be active after genesis with Feature account"
    );

    let treasury_before = bank.get_balance(&DEFAULT_TREASURY_PUBKEY);
    let capitalization_before = bank.capitalization();

    // Process a simple transfer to generate fees.
    let recipient = Pubkey::new_unique();
    let tx = system_transaction::transfer(
        &mint_keypair,
        &recipient,
        1_000,
        bank.last_blockhash(),
    );
    assert!(bank.process_transaction(&tx).is_ok());

    // The fee paid is determined by the fee rate governor.
    let fee_paid = bank.get_lamports_per_signature();
    assert!(fee_paid > 0, "Fee must be non-zero for this test");

    // Freeze the bank — this calls distribute_transaction_fee_details().
    goto_end_of_slot(bank.clone());

    // Compute expected split at epoch 0 (launch: 10% burn, 0% validator, 45% treasury, 45% dev).
    let expected_split = bank.projectdawn_config.split_fees(fee_paid, bank.epoch());
    assert_eq!(bank.epoch(), 0);

    // Treasury receives both its direct share AND the developer fallback
    // (system transfers have no program fee attribution, so developer share goes to treasury).
    let treasury_after = bank.get_balance(&DEFAULT_TREASURY_PUBKEY);
    let expected_treasury_total = expected_split.treasury + expected_split.developer;
    assert_eq!(
        treasury_after - treasury_before,
        expected_treasury_total,
        "Treasury should receive treasury + developer fallback (45% + 45% = 90%) at launch"
    );

    // Capitalization should decrease by the burn amount (plus a small sysvar delta).
    // The sysvar_and_builtin_program_delta accounts for account creation during bank init.
    let sysvar_and_builtin_program_delta = 1;
    assert_eq!(
        capitalization_before - expected_split.burn + sysvar_and_builtin_program_delta,
        bank.capitalization(),
        "Capitalization should decrease by burn amount"
    );

    // At launch, validator share is 0%.
    assert_eq!(expected_split.validator, 0, "Validator share is 0% at launch");
}

// ---------------------------------------------------------------------------
// Test 2: 4-way fee split at maturity (epoch >= 1460)
// ---------------------------------------------------------------------------
#[test]
fn test_fee_split_4way_at_maturity() {
    let leader = Pubkey::new_unique();
    let mint_lamports = 100_000_000_000;
    let GenesisConfigInfo {
        mut genesis_config,
        mint_keypair,
        ..
    } = create_genesis_config_with_leader(mint_lamports, &leader, 3);

    // Non-zero fees.
    genesis_config.fee_rate_governor = FeeRateGovernor::new(5000, 0);

    let rent = Rent::default();

    // Use a custom epoch schedule so we can reach epoch 1460 at a reasonable slot.
    // With 32 slots/epoch, epoch 1460 starts at slot 32 * 1460 = 46720.
    let epoch_schedule = EpochSchedule::custom(32, 32, false);
    genesis_config.epoch_schedule = epoch_schedule;

    // Treasury account.
    let treasury_balance = rent.minimum_balance(0);
    genesis_config.accounts.insert(
        DEFAULT_TREASURY_PUBKEY,
        Account::from(AccountSharedData::new(treasury_balance, 0, &system_program::id())),
    );

    // Feature account for PROJECTDAWN_FEATURE_ID (pre-activated).
    let feature_balance = std::cmp::max(rent.minimum_balance(Feature::size_of()), 1);
    genesis_config.accounts.insert(
        PROJECTDAWN_FEATURE_ID,
        Account::from(feature::create_account(&Feature { activated_at: Some(0) }, feature_balance)),
    );

    let (bank, bank_forks) = Bank::new_with_bank_forks_for_tests(&genesis_config);
    assert!(bank.projectdawn_enabled);

    // Warp to a slot in epoch 1460 (maturity).
    let warped_slot: Slot = 32 * 1460;
    let warped_bank = Bank::warp_from_parent(
        bank.clone(),
        &Pubkey::default(),
        warped_slot,
        CalcAccountsHashDataSource::IndexForTests,
    );
    let warped_bank = bank_forks
        .write()
        .unwrap()
        .insert(warped_bank)
        .clone_without_scheduler();

    // warp_from_parent freezes the bank, so create a child for processing transactions.
    let maturity_slot = warped_slot + 1;
    let maturity_bank = new_bank_from_parent_with_bank_forks(
        &bank_forks,
        warped_bank,
        &Pubkey::default(),
        maturity_slot,
    );

    assert!(maturity_bank.projectdawn_enabled);
    let maturity_epoch = maturity_bank.epoch();
    assert!(
        maturity_epoch >= 1460,
        "Should be at maturity epoch, got {}",
        maturity_epoch
    );

    let treasury_before = maturity_bank.get_balance(&DEFAULT_TREASURY_PUBKEY);
    let capitalization_before = maturity_bank.capitalization();

    // Process a transfer to generate fees.
    let recipient = Pubkey::new_unique();
    let tx = system_transaction::transfer(
        &mint_keypair,
        &recipient,
        1_000,
        maturity_bank.last_blockhash(),
    );
    assert!(maturity_bank.process_transaction(&tx).is_ok());

    let fee_paid = maturity_bank.get_lamports_per_signature();

    goto_end_of_slot(maturity_bank.clone());

    // At maturity: 25% burn, 25% validator, 25% treasury, 25% developer.
    let expected_split = maturity_bank
        .projectdawn_config
        .split_fees(fee_paid, maturity_epoch);

    // Treasury receives its share + developer fallback (no program attribution for transfers).
    let treasury_after = maturity_bank.get_balance(&DEFAULT_TREASURY_PUBKEY);
    let expected_treasury_total = expected_split.treasury + expected_split.developer;
    assert_eq!(
        treasury_after - treasury_before,
        expected_treasury_total,
        "Treasury should receive treasury + developer fallback (25% + 25% = 50%) at maturity"
    );

    // At maturity all four buckets are 25%.
    assert_eq!(
        expected_split.burn,
        fee_paid / 4,
        "Burn should be ~25% of fees"
    );
    assert_eq!(
        expected_split.validator,
        fee_paid / 4,
        "Validator should be ~25% of fees"
    );

    // Capitalization decreases by at least the burn amount.
    // At epoch boundaries, sysvar updates can add small deltas, so we check
    // the capitalization dropped by approximately the burn amount.
    let cap_delta = capitalization_before.saturating_sub(maturity_bank.capitalization());
    assert!(
        cap_delta >= expected_split.burn,
        "Capitalization should decrease by at least the burn amount ({} burned, cap delta {})",
        expected_split.burn,
        cap_delta
    );
}

// ---------------------------------------------------------------------------
// Test 3: Feature gate disabled — stock Solana 2-way split
// ---------------------------------------------------------------------------
#[test]
fn test_feature_gate_disabled() {
    let leader = Pubkey::new_unique();
    let mint_lamports = 100_000_000_000;
    let GenesisConfigInfo {
        mut genesis_config,
        mint_keypair,
        ..
    } = create_genesis_config_with_leader(mint_lamports, &leader, 3);

    // Non-zero fees.
    genesis_config.fee_rate_governor = FeeRateGovernor::new(5000, 0);

    let rent = Rent::default();

    // Add treasury account but do NOT add the Feature account for PROJECTDAWN_FEATURE_ID.
    let treasury_balance = rent.minimum_balance(0);
    genesis_config.accounts.insert(
        DEFAULT_TREASURY_PUBKEY,
        Account::from(AccountSharedData::new(treasury_balance, 0, &system_program::id())),
    );

    let (bank, _bank_forks) = Bank::new_with_bank_forks_for_tests(&genesis_config);

    // ProjectDawn should NOT be enabled.
    assert!(
        !bank.projectdawn_enabled,
        "ProjectDawn should be disabled without Feature account"
    );

    let treasury_before = bank.get_balance(&DEFAULT_TREASURY_PUBKEY);
    let leader_before = bank.get_balance(&leader);

    // Process a transfer to generate fees.
    let recipient = Pubkey::new_unique();
    let tx = system_transaction::transfer(
        &mint_keypair,
        &recipient,
        1_000,
        bank.last_blockhash(),
    );
    assert!(bank.process_transaction(&tx).is_ok());

    goto_end_of_slot(bank.clone());

    let treasury_after = bank.get_balance(&DEFAULT_TREASURY_PUBKEY);
    let leader_after = bank.get_balance(&leader);

    // Treasury should NOT receive any fees (stock Solana: burn + validator only).
    assert_eq!(
        treasury_before, treasury_after,
        "Treasury should not receive fees when ProjectDawn is disabled"
    );

    // Leader (validator) should receive the deposit portion of the stock 2-way split.
    assert!(
        leader_after > leader_before,
        "Leader should receive fees in stock Solana split"
    );
}

// ---------------------------------------------------------------------------
// Test 4: Config round-trip — child bank inherits parent's ProjectDawnConfig
// ---------------------------------------------------------------------------
#[test]
fn test_snapshot_round_trip() {
    let (parent_bank, bank_forks, _mint_keypair) = setup_projectdawn_bank();

    // Verify parent has default config.
    let parent_config: &ProjectDawnConfig = &parent_bank.projectdawn_config;
    assert_eq!(parent_config.staking_rate_bp, 500);
    assert_eq!(parent_config.treasury_pubkey, DEFAULT_TREASURY_PUBKEY);
    assert_eq!(parent_config.launch_fee_split, [1000, 0, 4500, 4500]);
    assert_eq!(parent_config.target_fee_split, [2500, 2500, 2500, 2500]);
    assert_eq!(parent_config.transition_epochs, 1460);

    // Create a child bank from the parent.
    let child_bank = new_bank_from_parent_with_bank_forks(
        &bank_forks,
        parent_bank.clone(),
        &Pubkey::default(),
        parent_bank.slot() + 1,
    );

    // Child should inherit the exact same ProjectDawnConfig.
    let child_config: &ProjectDawnConfig = &child_bank.projectdawn_config;
    assert_eq!(
        *parent_config, *child_config,
        "Child bank must inherit parent's ProjectDawnConfig"
    );

    // Child should also inherit projectdawn_enabled.
    assert_eq!(
        parent_bank.projectdawn_enabled,
        child_bank.projectdawn_enabled,
        "Child bank must inherit parent's projectdawn_enabled flag"
    );

    // Create grandchild to verify multi-generation inheritance.
    let grandchild_bank = new_bank_from_parent_with_bank_forks(
        &bank_forks,
        child_bank.clone(),
        &Pubkey::default(),
        child_bank.slot() + 1,
    );
    let grandchild_config: &ProjectDawnConfig = &grandchild_bank.projectdawn_config;
    assert_eq!(
        *parent_config, *grandchild_config,
        "Grandchild bank must inherit original ProjectDawnConfig"
    );
}

// ---------------------------------------------------------------------------
// Test 5: Treasury account exists in genesis
// ---------------------------------------------------------------------------
#[test]
fn test_treasury_exists_in_genesis() {
    let (bank, _bank_forks, _mint_keypair) = setup_projectdawn_bank();

    // The treasury account should exist on the bank.
    let treasury_account = bank
        .get_account(&DEFAULT_TREASURY_PUBKEY)
        .expect("Treasury account must exist in genesis");

    // It should be system-owned.
    assert_eq!(
        *treasury_account.owner(),
        system_program::id(),
        "Treasury account must be system-owned"
    );

    // It should have a non-zero balance (at least rent-exempt minimum).
    assert!(
        treasury_account.lamports() > 0,
        "Treasury account must have non-zero balance, got {}",
        treasury_account.lamports()
    );
}
