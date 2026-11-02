use anchor_lang::prelude::*;
use anchor_lang::system_program::{transfer, Transfer};

declare_id!("V1beEscrow111111111111111111111111111111111");

pub const MAX_METADATA_LEN: usize = 64;
pub const BASIS_POINTS_DIVISOR: u64 = 10_000;

#[program]
pub mod vibe_escrow {
    use super::*;

    /// Initializes global protocol configuration.
    pub fn initialize_global_config(
        ctx: Context<InitializeGlobalConfig>,
        fee_basis_points: u16,
    ) -> Result<()> {
        require!(fee_basis_points <= 1000, VibeError::FeeExceedsMaximum); // Max 10%
        let config = &mut ctx.accounts.global_config;
        config.admin = ctx.accounts.admin.key();
        config.treasury = ctx.accounts.treasury.key();
        config.fee_basis_points = fee_basis_points;
        config.bump = ctx.bumps.global_config;
        emit!(GlobalConfigInitialized {
            admin: config.admin,
            treasury: config.treasury,
            fee_basis_points,
        });
        Ok(())
    }

    /// Task creator deposits SOL and specifies criteria, expiration, and oracle verification key.
    pub fn create_task(
        ctx: Context<CreateTask>,
        task_id: u64,
        reward_amount: u64,
        task_timeout_seconds: i64,
        expected_commit_hash: [u8; 32],
        metadata_uri: String,
    ) -> Result<()> {
        require!(reward_amount > 0, VibeError::ZeroRewardAmount);
        require!(
            task_timeout_seconds >= 60 && task_timeout_seconds <= 604800,
            VibeError::InvalidTimeoutWindow
        );
        require!(
            metadata_uri.len() <= MAX_METADATA_LEN,
            VibeError::MetadataUriTooLong
        );

        let clock = Clock::get()?;
        let task = &mut ctx.accounts.task_escrow;

        task.creator = ctx.accounts.creator.key();
        task.task_id = task_id;
        task.reward_amount = reward_amount;
        task.verification_oracle = ctx.accounts.verification_oracle.key();
        task.expected_commit_hash = expected_commit_hash;
        task.assigned_worker = Pubkey::default();
        task.created_at = clock.unix_timestamp;
        task.expires_at = clock
            .unix_timestamp
            .checked_add(task_timeout_seconds)
            .ok_or(VibeError::NumericalOverflow)?;
        task.state = TaskState::Open;
        task.bump = ctx.bumps.task_escrow;
        task.vault_bump = ctx.bumps.escrow_vault;
        task.metadata_uri = metadata_uri;

        // Escrow funds into program-controlled vault PDA
        let cpi_context = CpiContext::new(
            ctx.accounts.system_program.to_account_info(),
            Transfer {
                from: ctx.accounts.creator.to_account_info(),
                to: ctx.accounts.escrow_vault.to_account_info(),
            },
        );
        transfer(cpi_context, reward_amount)?;

        emit!(TaskCreated {
            task_id,
            creator: ctx.accounts.creator.key(),
            reward_amount,
            expires_at: task.expires_at,
        });

        Ok(())
    }

    /// Agent claims the task, preventing other agents from racing the completion.
    pub fn claim_task(ctx: Context<ClaimTask>, task_id: u64) -> Result<()> {
        let task = &mut ctx.accounts.task_escrow;
        let clock = Clock::get()?;

        require!(task.state == TaskState::Open, VibeError::InvalidTaskState);
        require!(clock.unix_timestamp < task.expires_at, VibeError::TaskExpired);

        task.assigned_worker = ctx.accounts.worker.key();
        task.state = TaskState::Claimed;

        emit!(TaskClaimed {
            task_id,
            worker: task.assigned_worker,
        });

        Ok(())
    }

    /// Settles the task: Worker provides commit artifact hash, co-signed by verification oracle.
    pub fn verify_and_settle(
        ctx: Context<VerifyAndSettle>,
        task_id: u64,
        execution_commit_hash: [u8; 32],
    ) -> Result<()> {
        let clock = Clock::get()?;
        let task = &mut ctx.accounts.task_escrow;
        let config = &ctx.accounts.global_config;

        require!(task.state == TaskState::Claimed, VibeError::InvalidTaskState);
        require!(clock.unix_timestamp <= task.expires_at, VibeError::TaskExpired);
        require_keys_eq!(
            task.assigned_worker,
            ctx.accounts.worker.key(),
            VibeError::UnauthorizedWorker
        );
        require_keys_eq!(
            task.verification_oracle,
            ctx.accounts.verification_oracle.key(),
            VibeError::UnauthorizedOracle
        );
        require!(
            task.expected_commit_hash == execution_commit_hash,
            VibeError::CommitHashMismatch
        );

        // Calculate platform fee and worker reward
        let reward_amount = task.reward_amount;
        let protocol_fee = reward_amount
            .checked_mul(config.fee_basis_points as u64)
            .ok_or(VibeError::NumericalOverflow)?
            .checked_div(BASIS_POINTS_DIVISOR)
            .ok_or(VibeError::NumericalOverflow)?;
        let worker_payout = reward_amount
            .checked_sub(protocol_fee)
            .ok_or(VibeError::NumericalOverflow)?;

        let task_key = task.key();
        let vault_bump = task.vault_bump;
        let vault_signer_seeds: &[&[&[u8]]] = &[&[
            b"escrow_vault",
            task_key.as_ref(),
            &[vault_bump],
        ]];

        // Transfer payout to worker
        if worker_payout > 0 {
            let cpi_worker_ctx = CpiContext::new_with_signer(
                ctx.accounts.system_program.to_account_info(),
                Transfer {
                    from: ctx.accounts.escrow_vault.to_account_info(),
                    to: ctx.accounts.worker.to_account_info(),
                },
                vault_signer_seeds,
            );
            transfer(cpi_worker_ctx, worker_payout)?;
        }

        // Transfer fee to protocol treasury
        if protocol_fee > 0 {
            let cpi_fee_ctx = CpiContext::new_with_signer(
                ctx.accounts.system_program.to_account_info(),
                Transfer {
                    from: ctx.accounts.escrow_vault.to_account_info(),
                    to: ctx.accounts.treasury.to_account_info(),
                },
                vault_signer_seeds,
            );
            transfer(cpi_fee_ctx, protocol_fee)?;
        }

        task.state = TaskState::Completed;

        emit!(TaskSettled {
            task_id,
            worker: ctx.accounts.worker.key(),
            payout: worker_payout,
            fee: protocol_fee,
        });

        Ok(())
    }

    /// Task creator refunds escrow if worker failed to deliver within time bounds.
    pub fn cancel_or_refund(ctx: Context<CancelOrRefund>, _task_id: u64) -> Result<()> {
        let clock = Clock::get()?;
        let task = &mut ctx.accounts.task_escrow;

        // Allow cancellation if task expired or if never claimed
        let is_expired = clock.unix_timestamp >= task.expires_at;
        let is_unclaimed = task.state == TaskState::Open;

        require!(
            is_expired || is_unclaimed,
            VibeError::TaskCannotBeCancelled
        );
        require!(
            task.state != TaskState::Completed && task.state != TaskState::Refunded,
            VibeError::InvalidTaskState
        );

        let task_key = task.key();
        let vault_bump = task.vault_bump;
        let vault_signer_seeds: &[&[&[u8]]] = &[&[
            b"escrow_vault",
            task_key.as_ref(),
            &[vault_bump],
        ]];

        let vault_lamports = ctx.accounts.escrow_vault.to_account_info().lamports();

        if vault_lamports > 0 {
            let cpi_refund_ctx = CpiContext::new_with_signer(
                ctx.accounts.system_program.to_account_info(),
                Transfer {
                    from: ctx.accounts.escrow_vault.to_account_info(),
                    to: ctx.accounts.creator.to_account_info(),
                },
                vault_signer_seeds,
            );
            transfer(cpi_refund_ctx, vault_lamports)?;
        }

        task.state = TaskState::Refunded;

        emit!(TaskRefunded {
            task_id: task.task_id,
            creator: ctx.accounts.creator.key(),
            refunded_amount: vault_lamports,
        });

        Ok(())
    }
}

// -----------------------------------------------------------------------------
// ACCOUNT CONTEXTS
// -----------------------------------------------------------------------------

#[derive(Accounts)]
pub fn initialize_global_config_context<'info>(
    admin: &Signer<'info>,
    treasury: &AccountInfo<'info>,
    global_config: &Account<'info, GlobalConfig>,
    system_program: &Program<'info, System>,
) -> InitializeGlobalConfig<'info> {
    InitializeGlobalConfig {
        admin: admin.clone(),
        treasury: treasury.clone(),
        global_config: global_config.clone(),
        system_program: system_program.clone(),
    }
}

#[derive(Accounts)]
pub struct InitializeGlobalConfig<'info> {
    #[account(mut)]
    pub admin: Signer<'info>,
    /// CHECK: Protocol treasury recipient
    pub treasury: AccountInfo<'info>,
    #[account(
        init,
        payer = admin,
        space = GlobalConfig::SPACE,
        seeds = [b"global_config"],
        bump
    )]
    pub global_config: Account<'info, GlobalConfig>,
    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
#[instruction(task_id: u64)]
pub struct CreateTask<'info> {
    #[account(mut)]
    pub creator: Signer<'info>,
    /// CHECK: Public key of the trusted oracle/TEE verifier
    pub verification_oracle: AccountInfo<'info>,
    #[account(
        init,
        payer = creator,
        space = TaskEscrow::SPACE,
        seeds = [b"task_escrow", creator.key().as_ref(), &task_id.to_le_bytes()],
        bump
    )]
    pub task_escrow: Account<'info, TaskEscrow>,
    /// CHECK: Program-derived vault holding the SOL reward
    #[account(
        mut,
        seeds = [b"escrow_vault", task_escrow.key().as_ref()],
        bump
    )]
    pub escrow_vault: AccountInfo<'info>,
    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
#[instruction(task_id: u64)]
pub struct ClaimTask<'info> {
    #[account(mut)]
    pub worker: Signer<'info>,
    #[account(
        mut,
        seeds = [b"task_escrow", task_escrow.creator.as_ref(), &task_id.to_le_bytes()],
        bump = task_escrow.bump
    )]
    pub task_escrow: Account<'info, TaskEscrow>,
}

#[derive(Accounts)]
#[instruction(task_id: u64)]
pub struct VerifyAndSettle<'info> {
    #[account(mut)]
    pub worker: Signer<'info>,
    pub verification_oracle: Signer<'info>,
    #[account(
        mut,
        seeds = [b"global_config"],
        bump = global_config.bump
    )]
    pub global_config: Account<'info, GlobalConfig>,
    #[account(
        mut,
        seeds = [b"task_escrow", task_escrow.creator.as_ref(), &task_id.to_le_bytes()],
        bump = task_escrow.bump
    )]
    pub task_escrow: Account<'info, TaskEscrow>,
    /// CHECK: Validated against task_escrow and seeds
    #[account(
        mut,
        seeds = [b"escrow_vault", task_escrow.key().as_ref()],
        bump = task_escrow.vault_bump
    )]
    pub escrow_vault: AccountInfo<'info>,
    /// CHECK: Validated against global_config.treasury
    #[account(
        mut,
        address = global_config.treasury @ VibeError::InvalidTreasuryAccount
    )]
    pub treasury: AccountInfo<'info>,
    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
#[instruction(task_id: u64)]
pub struct CancelOrRefund<'info> {
    #[account(
        mut,
        address = task_escrow.creator @ VibeError::UnauthorizedCreator
    )]
    pub creator: Signer<'info>,
    #[account(
        mut,
        seeds = [b"task_escrow", creator.key().as_ref(), &task_id.to_le_bytes()],
        bump = task_escrow.bump,
        close = creator
    )]
    pub task_escrow: Account<'info, TaskEscrow>,
    /// CHECK: Vault matching escrow PDA
    #[account(
        mut,
        seeds = [b"escrow_vault", task_escrow.key().as_ref()],
        bump = task_escrow.vault_bump
    )]
    pub escrow_vault: AccountInfo<'info>,
    pub system_program: Program<'info, System>,
}

// -----------------------------------------------------------------------------
// STATE DEFINITIONS
// -----------------------------------------------------------------------------

#[account]
pub struct GlobalConfig {
    pub admin: Pubkey,          // 32
    pub treasury: Pubkey,       // 32
    pub fee_basis_points: u16,  // 2
    pub bump: u8,               // 1
}

impl GlobalConfig {
    pub const SPACE: usize = 8 + 32 + 32 + 2 + 1;
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, PartialEq, Eq)]
pub enum TaskState {
    Open,
    Claimed,
    Completed,
    Refunded,
}

#[account]
pub struct TaskEscrow {
    pub creator: Pubkey,               // 32
    pub task_id: u64,                  // 8
    pub reward_amount: u64,            // 8
    pub verification_oracle: Pubkey,   // 32
    pub expected_commit_hash: [u8; 32],// 32
    pub assigned_worker: Pubkey,       // 32
    pub created_at: i64,               // 8
    pub expires_at: i64,               // 8
    pub state: TaskState,              // 1
    pub bump: u8,                      // 1
    pub vault_bump: u8,                // 1
    pub metadata_uri: String,          // 4 + 64
}

impl TaskEscrow {
    pub const SPACE: usize = 8 + 32 + 8 + 8 + 32 + 32 + 32 + 8 + 8 + 1 + 1 + 1 + (4 + MAX_METADATA_LEN);
}

// -----------------------------------------------------------------------------
// EVENTS & ERRORS
// -----------------------------------------------------------------------------

#[event]
pub struct GlobalConfigInitialized {
    pub admin: Pubkey,
    pub treasury: Pubkey,
    pub fee_basis_points: u16,
}

#[event]
pub struct TaskCreated {
    pub task_id: u64,
    pub creator: Pubkey,
    pub reward_amount: u64,
    pub expires_at: i64,
}

#[event]
pub struct TaskClaimed {
    pub task_id: u64,
    pub worker: Pubkey,
}

#[event]
pub struct TaskSettled {
    pub task_id: u64,
    pub worker: Pubkey,
    pub payout: u64,
    pub fee: u64,
}

#[event]
pub struct TaskRefunded {
    pub task_id: u64,
    pub creator: Pubkey,
    pub refunded_amount: u64,
}

#[error_code]
pub enum VibeError {
    #[msg("Fee basis points cannot exceed 1000 (10%)")]
    FeeExceedsMaximum,
    #[msg("Reward amount must be greater than zero")]
    ZeroRewardAmount,
    #[msg("Timeout window must be between 60 seconds and 7 days")]
    InvalidTimeoutWindow,
    #[msg("Metadata URI exceeds maximum length of 64 bytes")]
    MetadataUriTooLong,
    #[msg("Task is not in the required state for this operation")]
    InvalidTaskState,
    #[msg("Task has already expired")]
    TaskExpired,
    #[msg("Worker signer does not match assigned worker")]
    UnauthorizedWorker,
    #[msg("Oracle signer does not match task verification oracle")]
    UnauthorizedOracle,
    #[msg("Commit hash does not match expected result")]
    CommitHashMismatch,
    #[msg("Numerical overflow occurred in calculations")]
    NumericalOverflow,
    #[msg("Treasury account does not match global config")]
    InvalidTreasuryAccount,
    #[msg("Task cannot be cancelled: still active and claimed")]
    TaskCannotBeCancelled,
    #[msg("Signer is not authorized to cancel this task")]
    UnauthorizedCreator,
}