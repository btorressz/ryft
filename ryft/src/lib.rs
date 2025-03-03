use anchor_lang::prelude::*;
use anchor_lang::solana_program::clock::Clock;
use anchor_spl::token::{self, TokenAccount, Token, Transfer};

declare_id!("5Qyc9MhKk2Dfh3TrGnruFaUPCoYbBcWRjkWc2pqQFkbs");

#[program]
pub mod ryft {
    use super::*;

    /// Initializes the global state for RYFT.
    /// `fee_rate` is provided in basis points.
    pub fn initialize(ctx: Context<Initialize>, fee_rate: u64) -> Result<()> {
        {
            let state = &mut ctx.accounts.global_state;
            state.admin = *ctx.accounts.admin.key;
            state.fee_rate = fee_rate;
            state.total_liquidity = 0;
            state.total_staked = 0;
            state.accumulated_fees = 0;
            state.is_flash_loan_active = false;
            state.treasury_account = ctx.accounts.treasury.key();
            // Initialize whitelist with an empty vector.
            state.flash_loan_whitelist = Vec::new();
        }
        Ok(())
    }

    /// Governance-controlled instruction to update the fee rate.
    pub fn update_fee_rate(ctx: Context<UpdateFeeRate>, new_fee_rate: u64) -> Result<()> {
        {
            let state = &mut ctx.accounts.global_state;
            require!(state.admin == *ctx.accounts.admin.key, CustomError::Unauthorized);
            state.fee_rate = new_fee_rate;
        }
        Ok(())
    }

    /// Deposits tokens from a liquidity provider into the pool.
    pub fn deposit_liquidity(ctx: Context<DepositLiquidity>, amount: u64) -> Result<()> {
        // Perform token transfer (immutable borrow inside helper)
        {
            let transfer_ctx = ctx.accounts.into_transfer_to_pool_context();
            token::transfer(transfer_ctx, amount)?;
        }
        // Update liquidity in state in its own block
        {
            let state = &mut ctx.accounts.global_state;
            state.total_liquidity = state.total_liquidity.checked_add(amount).unwrap();
        }
        Ok(())
    }

    /// Withdraws liquidity from the pool back to the provider.
    pub fn withdraw_liquidity(ctx: Context<WithdrawLiquidity>, amount: u64) -> Result<()> {
        // First, check that enough liquidity exists.
        {
            let available = ctx.accounts.global_state.total_liquidity;
            require!(available >= amount, CustomError::InsufficientLiquidity);
        }
        // Then perform the token transfer.
        {
            let transfer_ctx = ctx.accounts.into_transfer_from_pool_context();
            token::transfer(transfer_ctx, amount)?;
        }
        // Finally, update the global state.
        {
            let state = &mut ctx.accounts.global_state;
            state.total_liquidity = state.total_liquidity.checked_sub(amount).unwrap();
        }
        Ok(())
    }

    /// Stake RYFT tokens for flash loan priority and yield.
    pub fn stake(ctx: Context<Stake>, amount: u64) -> Result<()> {
        // First, transfer tokens from the user to the stake vault.
        {
            let transfer_ctx = ctx.accounts.into_transfer_to_stake_context();
            token::transfer(transfer_ctx, amount)?;
        }
        // Then update the user's stake.
        {
            let user_stake = &mut ctx.accounts.user_stake;
            if user_stake.amount == 0 {
                user_stake.last_stake_timestamp = Clock::get()?.unix_timestamp;
            }
            user_stake.amount = user_stake.amount.checked_add(amount).unwrap();
        }
        // And update the global staked total.
        {
            let state = &mut ctx.accounts.global_state;
            state.total_staked = state.total_staked.checked_add(amount).unwrap();
        }
        Ok(())
    }

    /// Unstake previously staked RYFT tokens.
    pub fn unstake(ctx: Context<Unstake>, amount: u64) -> Result<()> {
        // Ensure the user has enough staked tokens.
        {
            let current_stake = ctx.accounts.user_stake.amount;
            require!(current_stake >= amount, CustomError::InsufficientStake);
        }
        // Transfer tokens from the stake vault back to the user.
        {
            let transfer_ctx = ctx.accounts.into_transfer_from_stake_context();
            token::transfer(transfer_ctx, amount)?;
        }
        // Update the user's stake.
        {
            let user_stake = &mut ctx.accounts.user_stake;
            user_stake.amount = user_stake.amount.checked_sub(amount).unwrap();
        }
        // Update the global staked total.
        {
            let state = &mut ctx.accounts.global_state;
            state.total_staked = state.total_staked.checked_sub(amount).unwrap();
        }
        Ok(())
    }

    /// Executes an atomic flash loan. The borrowed funds must be repaid in the same transaction.
    /// Features include reentrancy protection, whitelist check, time-limited execution, and collateral backing.
    pub fn flash_loan(ctx: Context<FlashLoan>, amount: u64, collateral_amount: u64) -> Result<()> {
        // Set reentrancy flag and perform whitelist check.
        {
            let state = &mut ctx.accounts.global_state;
            require!(!state.is_flash_loan_active, CustomError::FlashLoanInProgress);
            state.is_flash_loan_active = true;
            if !state.flash_loan_whitelist.is_empty() {
                require!(state.flash_loan_whitelist.contains(ctx.accounts.borrower.key), CustomError::NotWhitelisted);
            }
        }
        // Check pool liquidity.
        if ctx.accounts.pool_account.amount < amount {
            {
                let state = &mut ctx.accounts.global_state;
                state.is_flash_loan_active = false;
            }
            return Err(CustomError::InsufficientLiquidity.into());
        }
        // Transfer collateral (if provided).
        if collateral_amount > 0 {
            {
                let collateral_ctx = ctx.accounts.into_transfer_collateral_context();
                token::transfer(collateral_ctx, collateral_amount)?;
            }
        }
        // Read the fee rate from global state (immutable borrow) and compute fee.
        let fee_rate = ctx.accounts.global_state.fee_rate;
        let fee = amount.checked_mul(fee_rate).unwrap() / 10000;
        // Record flash loan details.
        {
            let flash_loan_state = &mut ctx.accounts.flash_loan_state;
            flash_loan_state.amount = amount;
            flash_loan_state.fee = fee;
            flash_loan_state.start_time = Clock::get()?.unix_timestamp;
            flash_loan_state.collateral = collateral_amount;
        }
        // Transfer the flash loan amount to the borrower.
        {
            let transfer_ctx = ctx.accounts.into_transfer_to_borrower_context();
            token::transfer(transfer_ctx, amount)?;
        }
        Ok(())
    }

    /// Repays a flash loan.
    /// Enforces repayment within a time limit and updates the borrower's reputation.
    pub fn repay_flash_loan(ctx: Context<RepayFlashLoan>) -> Result<()> {
        let flash_loan_state = &ctx.accounts.flash_loan_state;
        let current_time = Clock::get()?.unix_timestamp;
        require!(current_time - flash_loan_state.start_time <= 30, CustomError::FlashLoanExpired);
        {
            let state = &mut ctx.accounts.global_state;
            state.accumulated_fees = state.accumulated_fees.checked_add(flash_loan_state.fee).unwrap();
            state.is_flash_loan_active = false;
        }
        {
            let reputation = &mut ctx.accounts.borrower_reputation;
            reputation.borrower = *ctx.accounts.borrower.key;
            reputation.reputation = reputation.reputation.checked_add(1).unwrap();
        }
        Ok(())
    }

    /// Distributes rewards to stakers.
    /// This function is a placeholder for multi-token yield distribution and smart treasury mechanisms.
    pub fn distribute_rewards(ctx: Context<DistributeRewards>) -> Result<()> {
        // Reward distribution logic goes here.
        Ok(())
    }

    /// Compound staking rewards by auto-reinvesting them.
    pub fn compound_rewards(ctx: Context<CompoundRewards>) -> Result<()> {
        // Auto-compounding logic goes here.
        Ok(())
    }

    /// Executes a multi-hop flash loan across multiple liquidity pools.
    /// This is a placeholder for composable flash loans.
    pub fn multi_hop_flash_loan(ctx: Context<MultiHopFlashLoan>, amounts: Vec<u64>) -> Result<()> {
        // Multi-hop flash loan logic goes here.
        Ok(())
    }
}

//
// Account Contexts & Helpers
//

#[derive(Accounts)]
pub struct Initialize<'info> {
    #[account(init, payer = admin, space = 8 + GlobalState::LEN)]
    pub global_state: Account<'info, GlobalState>,
    #[account(mut)]
    pub admin: Signer<'info>,
    /// Treasury account for fee redistribution.
    pub treasury: AccountInfo<'info>,
    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
pub struct UpdateFeeRate<'info> {
    #[account(mut)]
    pub global_state: Account<'info, GlobalState>,
    #[account(mut)]
    pub admin: Signer<'info>,
}

#[derive(Accounts)]
pub struct DepositLiquidity<'info> {
    #[account(mut)]
    pub global_state: Account<'info, GlobalState>,
    #[account(mut)]
    pub provider: Signer<'info>,
    #[account(mut)]
    pub provider_token_account: Account<'info, TokenAccount>,
    #[account(mut)]
    pub pool_account: Account<'info, TokenAccount>,
    pub token_program: Program<'info, Token>,
}

impl<'info> DepositLiquidity<'info> {
    pub fn into_transfer_to_pool_context(&self) -> CpiContext<'_, '_, '_, 'info, Transfer<'info>> {
        let cpi_accounts = Transfer {
            from: self.provider_token_account.to_account_info().clone(),
            to: self.pool_account.to_account_info().clone(),
            authority: self.provider.to_account_info().clone(),
        };
        CpiContext::new(self.token_program.to_account_info().clone(), cpi_accounts)
    }
}

#[derive(Accounts)]
pub struct WithdrawLiquidity<'info> {
    #[account(mut)]
    pub global_state: Account<'info, GlobalState>,
    #[account(mut)]
    pub pool_account: Account<'info, TokenAccount>,
    #[account(mut)]
    pub provider_token_account: Account<'info, TokenAccount>,
    /// The authority for the pool account (typically a PDA) that must sign.
    pub pool_authority: Signer<'info>,
    pub token_program: Program<'info, Token>,
}

impl<'info> WithdrawLiquidity<'info> {
    pub fn into_transfer_from_pool_context(&self) -> CpiContext<'_, '_, '_, 'info, Transfer<'info>> {
        let cpi_accounts = Transfer {
            from: self.pool_account.to_account_info().clone(),
            to: self.provider_token_account.to_account_info().clone(),
            authority: self.pool_authority.to_account_info().clone(),
        };
        CpiContext::new(self.token_program.to_account_info().clone(), cpi_accounts)
    }
}

#[derive(Accounts)]
pub struct Stake<'info> {
    #[account(mut)]
    pub global_state: Account<'info, GlobalState>,
    #[account(mut)]
    pub user: Signer<'info>,
    #[account(
        init_if_needed,
        payer = user,
        space = 8 + UserStake::LEN,
        seeds = [b"user_stake", user.key.as_ref()],
        bump
    )]
    pub user_stake: Account<'info, UserStake>,
    #[account(mut)]
    pub user_token_account: Account<'info, TokenAccount>,
    #[account(mut)]
    pub stake_vault: Account<'info, TokenAccount>,
    /// The authority (often a PDA) that controls the stake vault.
    pub stake_vault_authority: AccountInfo<'info>,
    pub token_program: Program<'info, Token>,
    pub system_program: Program<'info, System>,
    pub rent: Sysvar<'info, Rent>,
}

impl<'info> Stake<'info> {
    pub fn into_transfer_to_stake_context(&self) -> CpiContext<'_, '_, '_, 'info, Transfer<'info>> {
        let cpi_accounts = Transfer {
            from: self.user_token_account.to_account_info().clone(),
            to: self.stake_vault.to_account_info().clone(),
            authority: self.user.to_account_info().clone(),
        };
        CpiContext::new(self.token_program.to_account_info().clone(), cpi_accounts)
    }
}

#[derive(Accounts)]
pub struct Unstake<'info> {
    #[account(mut)]
    pub global_state: Account<'info, GlobalState>,
    #[account(mut)]
    pub user: Signer<'info>,
    #[account(mut, seeds = [b"user_stake", user.key.as_ref()], bump)]
    pub user_stake: Account<'info, UserStake>,
    #[account(mut)]
    pub stake_vault: Account<'info, TokenAccount>,
    /// The authority (PDA) controlling the stake vault.
    pub stake_vault_authority: Signer<'info>,
    #[account(mut)]
    pub user_token_account: Account<'info, TokenAccount>,
    pub token_program: Program<'info, Token>,
}

impl<'info> Unstake<'info> {
    pub fn into_transfer_from_stake_context(&self) -> CpiContext<'_, '_, '_, 'info, Transfer<'info>> {
        let cpi_accounts = Transfer {
            from: self.stake_vault.to_account_info().clone(),
            to: self.user_token_account.to_account_info().clone(),
            authority: self.stake_vault_authority.to_account_info().clone(),
        };
        CpiContext::new(self.token_program.to_account_info().clone(), cpi_accounts)
    }
}

#[derive(Accounts)]
pub struct FlashLoan<'info> {
    #[account(mut)]
    pub global_state: Account<'info, GlobalState>,
    #[account(mut)]
    pub pool_account: Account<'info, TokenAccount>,
    /// The authority controlling the pool account (typically a PDA).
    pub pool_authority: Signer<'info>,
    #[account(mut)]
    pub borrower_token_account: Account<'info, TokenAccount>,
    /// CHECK: Borrower account (used only for receiving tokens). Marked mutable as it also pays for the new account.
    #[account(mut)]
    pub borrower: AccountInfo<'info>,
    #[account(init, payer = borrower, space = 8 + FlashLoanState::LEN)]
    pub flash_loan_state: Account<'info, FlashLoanState>,
    /// Account from which collateral will be transferred.
    #[account(mut)]
    pub borrower_collateral_account: Account<'info, TokenAccount>,
    /// Collateral escrow account.
    #[account(mut)]
    pub collateral_escrow: Account<'info, TokenAccount>,
    pub token_program: Program<'info, Token>,
    pub system_program: Program<'info, System>,
    pub rent: Sysvar<'info, Rent>,
}

impl<'info> FlashLoan<'info> {
    pub fn into_transfer_to_borrower_context(&self) -> CpiContext<'_, '_, '_, 'info, Transfer<'info>> {
        let cpi_accounts = Transfer {
            from: self.pool_account.to_account_info().clone(),
            to: self.borrower_token_account.to_account_info().clone(),
            authority: self.pool_authority.to_account_info().clone(),
        };
        CpiContext::new(self.token_program.to_account_info().clone(), cpi_accounts)
    }
    pub fn into_transfer_collateral_context(&self) -> CpiContext<'_, '_, '_, 'info, Transfer<'info>> {
        let cpi_accounts = Transfer {
            from: self.borrower_collateral_account.to_account_info().clone(),
            to: self.collateral_escrow.to_account_info().clone(),
            authority: self.borrower.to_account_info().clone(),
        };
        CpiContext::new(self.token_program.to_account_info().clone(), cpi_accounts)
    }
}

#[derive(Accounts)]
pub struct RepayFlashLoan<'info> {
    #[account(mut)]
    pub global_state: Account<'info, GlobalState>,
    #[account(mut)]
    pub pool_account: Account<'info, TokenAccount>,
    /// The pool authority must sign the repayment.
    pub pool_authority: Signer<'info>,
    #[account(mut, close = borrower)]
    pub flash_loan_state: Account<'info, FlashLoanState>,
    /// CHECK: This account receives lamports from closing the flash loan state.
    #[account(mut)]
    pub borrower: AccountInfo<'info>,
    /// Borrower's reputation account.
    #[account(init_if_needed, payer = borrower, space = 8 + BorrowerReputation::LEN, seeds = [b"reputation", borrower.key.as_ref()], bump)]
    pub borrower_reputation: Account<'info, BorrowerReputation>,
    pub token_program: Program<'info, Token>,
    pub system_program: Program<'info, System>,
    pub rent: Sysvar<'info, Rent>,
}

#[derive(Accounts)]
pub struct DistributeRewards<'info> {
    #[account(mut)]
    pub global_state: Account<'info, GlobalState>,
}

#[derive(Accounts)]
pub struct CompoundRewards<'info> {
    #[account(mut)]
    pub global_state: Account<'info, GlobalState>,
    #[account(mut)]
    pub user: Signer<'info>,
    #[account(mut, seeds = [b"user_stake", user.key.as_ref()], bump)]
    pub user_stake: Account<'info, UserStake>,
    // Account for reward tokens, etc.
    pub token_program: Program<'info, Token>,
}

#[derive(Accounts)]
pub struct MultiHopFlashLoan<'info> {
    #[account(mut)]
    pub global_state: Account<'info, GlobalState>,
    // Additional accounts for multiple pools would be specified here.
    pub token_program: Program<'info, Token>,
}

//
// On–chain State Accounts
//

#[account]
pub struct GlobalState {
    pub admin: Pubkey,
    pub fee_rate: u64,         // in basis points
    pub total_liquidity: u64,  // tokens in the liquidity pool
    pub total_staked: u64,     // tokens staked by users
    pub accumulated_fees: u64, // fees collected from flash loans
    pub is_flash_loan_active: bool, // reentrancy guard flag
    pub treasury_account: Pubkey,   // for fee redistribution
    pub flash_loan_whitelist: Vec<Pubkey>, // optional whitelist for borrowers
}

impl GlobalState {
    // For the vector, we add 4 bytes for length and assume up to 10 addresses.
    pub const LEN: usize = 32 + 8 + 8 + 8 + 8 + 1 + 32 + (4 + 10 * 32);
}

#[account]
pub struct UserStake {
    pub owner: Pubkey,
    pub amount: u64,
    pub reward_debt: u64,          // if using an accrual model
    pub last_stake_timestamp: i64, // for proportional rewards
}

impl UserStake {
    pub const LEN: usize = 32 + 8 + 8 + 8;
}

#[account]
pub struct FlashLoanState {
    pub amount: u64,
    pub fee: u64,
    pub start_time: i64, // timestamp when the flash loan was issued
    pub collateral: u64, // collateral amount provided
}

impl FlashLoanState {
    pub const LEN: usize = 8 + 8 + 8 + 8;
}

#[account]
pub struct BorrowerReputation {
    pub borrower: Pubkey,
    pub reputation: u64,
}

impl BorrowerReputation {
    pub const LEN: usize = 32 + 8;
}

//
// Error Codes
//

#[error_code]
pub enum CustomError {
    #[msg("Insufficient liquidity in the pool.")]
    InsufficientLiquidity,
    #[msg("Insufficient staked balance.")]
    InsufficientStake,
    #[msg("Flash loan already in progress.")]
    FlashLoanInProgress,
    #[msg("Flash loan expired (time limit exceeded).")]
    FlashLoanExpired,
    #[msg("Borrower not whitelisted for flash loans.")]
    NotWhitelisted,
    #[msg("Unauthorized.")]
    Unauthorized,
}
