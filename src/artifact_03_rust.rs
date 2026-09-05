use anchor_lang::prelude::*;
use anchor_lang::system_program::{transfer, Transfer};

declare_id!("VibeVau1t1111111111111111111111111111111111");

#[program]
pub mod vibe_vault {
    use super::*;

    /// Initializes a new VibeVault with configurable agent authority and rate bounds.
    pub fn initialize_vault(
        ctx: Context<InitializeVault>,
        vault_id: u64,
        max_rate_per_second: u64,
    ) -> Result<()> {
        let vault = &mut ctx.accounts.vault;
        vault.owner = ctx.accounts.owner.key();
        vault.agent_key = ctx.accounts.initial_agent.key();
        vault.vault_id = vault_id;
        vault.max_rate_per_second = max_rate_per_second;
        vault.total_deposited = 0;
        vault.total_streamed = 0;
        vault.bump = ctx.bumps.vault;
        vault.treasury_bump = ctx.bumps.treasury;

        emit!(VaultInitializedEvent {
            vault: vault.key(),
            owner: vault.owner,
            agent: vault.agent_key,
            vault_id,
            max_rate_per_second,
        });

        Ok(())
    }

    /// Deposits native SOL into the vault's PDA treasury.
    pub fn deposit_funds(ctx: Context<DepositFunds>, amount: u64) -> Result<()> {
        require!(amount > 0, VibeVaultError::InvalidZeroAmount);

        let cpi_context = CpiContext::new(
            ctx.accounts.system_program.to_account_info(),
            Transfer {
                from: ctx.accounts.funder.to_account_info(),
                to: ctx.accounts.treasury.to_account_info(),
            },
        );
        transfer(cpi_context, amount)?;

        let vault = &mut ctx.accounts.vault;
        vault.total_deposited = vault
            .total_deposited
            .checked_add(amount)
            .ok_or(VibeVaultError::MathOverflow)?;

        emit!(FundsDepositedEvent {
            vault: vault.key(),
            funder: ctx.accounts.funder.key(),
            amount,
        });

        Ok(())
    }

    /// Update the delegated AI Agent authority (e.g. key rotation on Replit).
    pub fn update_agent(ctx: Context<UpdateAgent>, new_agent: Pubkey) -> Result<()> {
        let vault = &mut ctx.accounts.vault;
        let old_agent = vault.agent_key;
        vault.agent_key = new_agent;

        emit!(AgentRotatedEvent {
            vault: vault.key(),
            old_agent,
            new_agent,
        });

        Ok(())
    }

    /// Starts a real-time linear stream to a designated recipient.
    /// Can only be invoked by either the Vault Owner or the Authorized Agent.
    pub fn create_stream(
        ctx: Context<CreateStream>,
        rate_per_second: u64,
        stream_end_time: i64,
    ) -> Result<()> {
        let clock = Clock::get()?;
        let current_time = clock.unix_timestamp;

        require!(
            stream_end_time > current_time,
            VibeVaultError::InvalidStreamDuration
        );
        require!(
            rate_per_second <= ctx.accounts.vault.max_rate_per_second,
            VibeVaultError::RateExceedsCap
        );

        let stream = &mut ctx.accounts.stream;
        stream.vault = ctx.accounts.vault.key();
        stream.recipient = ctx.accounts.recipient.key();
        stream.rate_per_second = rate_per_second;
        stream.start_time = current_time;
        stream.last_settled_time = current_time;
        stream.end_time = stream_end_time;
        stream.total_claimed = 0;
        stream.bump = ctx.bumps.stream;

        emit!(StreamCreatedEvent {
            vault: ctx.accounts.vault.key(),
            stream: stream.key(),
            recipient: stream.recipient,
            rate_per_second,
            start_time: current_time,
            end_time: stream_end_time,
        });

        Ok(())
    }

    /// Cranks an existing stream: calculates accrued unlocked lamports and transfers them.
    /// Fully permissionless; can be called by any keeper bot or the recipient.
    pub fn crank_stream(ctx: Context<CrankStream>) -> Result<()> {
        let clock = Clock::get()?;
        let current_time = clock.unix_timestamp;
        let stream = &mut ctx.accounts.stream;

        require!(
            stream.last_settled_time < stream.end_time,
            VibeVaultError::StreamAlreadyFinished
        );

        let effective_time = std::cmp::min(current_time, stream.end_time);
        let elapsed_seconds = effective_time
            .checked_sub(stream.last_settled_time)
            .ok_or(VibeVaultError::MathOverflow)? as u64;

        if elapsed_seconds == 0 {
            return Ok(());
        }

        let accrued_amount = elapsed_seconds
            .checked_mul(stream.rate_per_second)
            .ok_or(VibeVaultError::MathOverflow)?;

        require!(
            ctx.accounts.treasury.lamports() >= accrued_amount,
            VibeVaultError::InsufficientTreasuryBalance
        );

        stream.last_settled_time = effective_time;
        stream.total_claimed = stream
            .total_claimed
            .checked_add(accrued_amount)
            .ok_or(VibeVaultError::MathOverflow)?;

        // Perform PDA-signed native lamport disbursement
        let vault_key = ctx.accounts.vault.key();
        let treasury_bump = ctx.accounts.vault.treasury_bump;
        let signer_seeds: &[&[&[u8]]] = &[&[b"treasury", vault_key.as_ref(), &[treasury_bump]]];

        **ctx.accounts.treasury.try_borrow_mut_lamports()? = ctx
            .accounts
            .treasury
            .lamports()
            .checked_sub(accrued_amount)
            .ok_or(VibeVaultError::MathOverflow)?;

        **ctx.accounts.recipient.try_borrow_mut_lamports()? = ctx
            .accounts
            .recipient
            .lamports()
            .checked_add(accrued_amount)
            .ok_or(VibeVaultError::MathOverflow)?;

        let vault = &mut ctx.accounts.vault;
        vault.total_streamed = vault
            .total_streamed
            .checked_add(accrued_amount)
            .ok_or(VibeVaultError::MathOverflow)?;

        emit!(StreamCrankedEvent {
            stream: stream.key(),
            recipient: ctx.accounts.recipient.key(),
            disbursed_amount: accrued_amount,
            timestamp: current_time,
        });

        Ok(())
    }

    /// Closes a finalized stream PDA and reclaims rent storage lamports back to the owner.
    pub fn close_stream(ctx: Context<CloseStream>) -> Result<()> {
        let stream = &ctx.accounts.stream;
        let clock = Clock::get()?;

        require!(
            clock.unix_timestamp >= stream.end_time,
            VibeVaultError::StreamNotEnded
        );

        emit!(StreamClosedEvent {
            stream: stream.key(),
            owner_recipient: ctx.accounts.owner.key(),
        });

        Ok(())
    }
}

// -----------------------------------------------------------------------------
// Account Context Structs
// -----------------------------------------------------------------------------

#[derive(Accounts)]
#[instruction(vault_id: u64)]
pub struct InitializeVault<'info> {
    #[account(
        init,
        payer = owner,
        space = 8 + VaultState::INIT_SPACE,
        seeds = [b"vault", owner.key().as_ref(), &vault_id.to_le_bytes()],
        bump
    )]
    pub vault: Account<'info, VaultState>,

    /// CHECK: PDA Treasury account for holding native SOL escrow
    #[account(
        mut,
        seeds = [b"treasury", vault.key().as_ref()],
        bump
    )]
    pub treasury: SystemAccount<'info>,

    /// CHECK: May be any active agent pubkey (e.g., Replit Agent service key)
    pub initial_agent: UncheckedAccount<'info>,

    #[account(mut)]
    pub owner: Signer<'info>,

    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
pub struct DepositFunds<'info> {
    #[account(
        mut,
        has_one = owner @ VibeVaultError::UnauthorizedSigner
    )]
    pub vault: Account<'info, VaultState>,

    /// CHECK: PDA treasury receiving lamports
    #[account(
        mut,
        seeds = [b"treasury", vault.key().as_ref()],
        bump = vault.treasury_bump
    )]
    pub treasury: SystemAccount<'info>,

    #[account(mut)]
    pub funder: Signer<'info>,

    pub owner: SystemAccount<'info>,

    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
pub struct UpdateAgent<'info> {
    #[account(
        mut,
        has_one = owner @ VibeVaultError::UnauthorizedSigner
    )]
    pub vault: Account<'info, VaultState>,

    pub owner: Signer<'info>,
}

#[derive(Accounts)]
pub struct CreateStream<'info> {
    #[account(
        mut,
        constraint = (signer.key() == vault.owner || signer.key() == vault.agent_key) 
            @ VibeVaultError::UnauthorizedAgentOrOwner
    )]
    pub vault: Account<'info, VaultState>,

    #[account(
        init,
        payer = signer,
        space = 8 + StreamState::INIT_SPACE,
        seeds = [b"stream", vault.key().as_ref(), recipient.key().as_ref()],
        bump
    )]
    pub stream: Account<'info, StreamState>,

    /// CHECK: Recipient receiving stream settlements
    pub recipient: UncheckedAccount<'info>,

    #[account(mut)]
    pub signer: Signer<'info>,

    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
pub struct CrankStream<'info> {
    #[account(
        mut,
        seeds = [b"vault", vault.owner.as_ref(), &vault.vault_id.to_le_bytes()],
        bump = vault.bump
    )]
    pub vault: Account<'info, VaultState>,

    #[account(
        mut,
        seeds = [b"stream", vault.key().as_ref(), recipient.key().as_ref()],
        bump = stream.bump,
        has_one = vault @ VibeVaultError::VaultMismatch,
        has_one = recipient @ VibeVaultError::RecipientMismatch
    )]
    pub stream: Account<'info, StreamState>,

    /// CHECK: PDA Treasury distributing funds
    #[account(
        mut,
        seeds = [b"treasury", vault.key().as_ref()],
        bump = vault.treasury_bump
    )]
    pub treasury: SystemAccount<'info>,

    /// CHECK: Matches stream.recipient
    #[account(mut)]
    pub recipient: UncheckedAccount<'info>,

    /// Permissionless keeper / cranker paying for minimal CU
    pub cranker: Signer<'info>,
}

#[derive(Accounts)]
pub struct CloseStream<'info> {
    #[account(
        has_one = owner @ VibeVaultError::UnauthorizedSigner
    )]
    pub vault: Account<'info, VaultState>,

    #[account(
        mut,
        close = owner,
        seeds = [b"stream", vault.key().as_ref(), stream.recipient.as_ref()],
        bump = stream.bump,
        has_one = vault @ VibeVaultError::VaultMismatch
    )]
    pub stream: Account<'info, StreamState>,

    #[account(mut)]
    pub owner: Signer<'info>,
}

// -----------------------------------------------------------------------------
// State Structs
// -----------------------------------------------------------------------------

#[account]
#[derive(InitSpace)]
pub struct VaultState {
    pub owner: Pubkey,               // 32
    pub agent_key: Pubkey,           // 32
    pub vault_id: u64,               // 8
    pub max_rate_per_second: u64,    // 8
    pub total_deposited: u64,        // 8
    pub total_streamed: u64,         // 8
    pub bump: u8,                    // 1
    pub treasury_bump: u8,           // 1
}

#[account]
#[derive(InitSpace)]
pub struct StreamState {
    pub vault: Pubkey,               // 32
    pub recipient: Pubkey,           // 32
    pub rate_per_second: u64,        // 8
    pub start_time: i64,             // 8
    pub last_settled_time: i64,      // 8
    pub end_time: i64,               // 8
    pub total_claimed: u64,          // 8
    pub bump: u8,                    // 1
}

// -----------------------------------------------------------------------------
// Events
// -----------------------------------------------------------------------------

#[event]
pub struct VaultInitializedEvent {
    pub vault: Pubkey,
    pub owner: Pubkey,
    pub agent: Pubkey,
    pub vault_id: u64,
    pub max_rate_per_second: u64,
}

#[event]
pub struct FundsDepositedEvent {
    pub vault: Pubkey,
    pub funder: Pubkey,
    pub amount: u64,
}

#[event]
pub struct AgentRotatedEvent {
    pub vault: Pubkey,
    pub old_agent: Pubkey,
    pub new_agent: Pubkey,
}

#[event]
pub struct StreamCreatedEvent {
    pub vault: Pubkey,
    pub stream: Pubkey,
    pub recipient: Pubkey,
    pub rate_per_second: u64,
    pub start_time: i64,
    pub end_time: i64,
}

#[event]
pub struct StreamCrankedEvent {
    pub stream: Pubkey,
    pub recipient: Pubkey,
    pub disbursed_amount: u64,
    pub timestamp: i64,
}

#[event]
pub struct StreamClosedEvent {
    pub stream: Pubkey,
    pub owner_recipient: Pubkey,
}

// -----------------------------------------------------------------------------
// Error Codes
// -----------------------------------------------------------------------------

#[error_code]
pub enum VibeVaultError {
    #[msg("Amount passed must be strictly greater than zero.")]
    InvalidZeroAmount,
    #[msg("Signer is not authorized to act as owner for this vault.")]
    UnauthorizedSigner,
    #[msg("Signer is neither the designated agent nor the vault owner.")]
    UnauthorizedAgentOrOwner,
    #[msg("Requested streaming rate exceeds vault-configured maximum cap.")]
    RateExceedsCap,
    #[msg("Stream end time must be strictly in the future.")]
    InvalidStreamDuration,
    #[msg("Stream has already arrived at its terminal timestamp.")]
    StreamAlreadyFinished,
    #[msg("Treasury balance is insufficient to settle accrued stream lamports.")]
    InsufficientTreasuryBalance,
    #[msg("Stream cannot be closed until terminal end_time has passed.")]
    StreamNotEnded,
    #[msg("Provided stream account does not belong to the target vault.")]
    VaultMismatch,
    #[msg("Provided recipient account does not match registered stream recipient.")]
    RecipientMismatch,
    #[msg("Integer overflow occurred during time-decay arithmetic.")]
    MathOverflow,
}