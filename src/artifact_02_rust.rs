use anchor_lang::prelude::*;
use anchor_lang::system_program;

declare_id!("V1beMesh11111111111111111111111111111111111");

pub const TASK_SEED: &[u8] = b"task";
pub const VAULT_SEED: &[u8] = b"vault";
pub const CONFIG_SEED: &[u8] = b"config";

#[program]
pub mod vibemesh {
    use super::*;

    /// Initializes global protocol parameters (protocol fee, super-arbiter).
    pub fn initialize_protocol(
        ctx: Context<InitializeProtocol>,
        fee_basis_points: u16,
    ) -> Result<()> {
        require!(fee_basis_points <= 1000, VibeError::FeeExceedsMaximum); // Max 10%
        let config = &mut ctx.accounts.config;
        config.authority = ctx.accounts.authority.key();
        config.treasury = ctx.accounts.treasury.key();
        config.fee_basis_points = fee_basis_points;
        config.bump = ctx.bumps.config;
        config.total_tasks_created = 0;
        config.total_tasks_settled = 0;
        emit!(ProtocolInitialized {
            authority: config.authority,
            treasury: config.treasury,
            fee_basis_points,
        });
        Ok(())
    }

    /// Creates a funded task escrow with defined timeout and payout parameters.
    pub fn create_task(
        ctx: Context<CreateTask>,
        task_id: [u8; 16],
        reward_amount: u64,
        worker_stake_required: u64,
        execution_window: i64, // In seconds (max execution time after claim)
        challenge_window: i64, // In seconds (review window for dispute)
    ) -> Result<()> {
        require!(reward_amount > 0, VibeError::InvalidRewardAmount);
        require!(execution_window >= 60, VibeError::WindowTooShort);
        require!(challenge_window >= 30, VibeError::WindowTooShort);

        let task = &mut ctx.accounts.task;
        task.employer = ctx.accounts.employer.key();
        task.task_id = task_id;
        task.reward_amount = reward_amount;
        task.worker_stake_required = worker_stake_required;
        task.execution_window = execution_window;
        task.challenge_window = challenge_window;
        task.status = TaskStatus::Created;
        task.worker = Pubkey::default();
        task.claimed_at = 0;
        task.submitted_at = 0;
        task.output_hash = [0u8; 32];
        task.bump = ctx.bumps.task;
        task.vault_bump = ctx.bumps.vault;

        // Transfer bounty from employer to PDA vault
        let cpi_ctx = CpiContext::new(
            ctx.accounts.system_program.to_account_info(),
            system_program::Transfer {
                from: ctx.accounts.employer.to_account_info(),
                to: ctx.accounts.vault.to_account_info(),
            },
        );
        system_program::transfer(cpi_ctx, reward_amount)?;

        let config = &mut ctx.accounts.config;
        config.total_tasks_created = config.total_tasks_created.checked_add(1).unwrap();

        emit!(TaskCreated {
            task_id,
            employer: task.employer,
            reward_amount,
            worker_stake_required,
        });

        Ok(())
    }

    /// Autonomous worker agent claims task and posts security bond.
    pub fn claim_task(ctx: Context<ClaimTask>) -> Result<()> {
        let task = &mut ctx.accounts.task;
        require!(task.status == TaskStatus::Created, VibeError::InvalidTaskStatus);

        let clock = Clock::get()?;

        if task.worker_stake_required > 0 {
            let cpi_ctx = CpiContext::new(
                ctx.accounts.system_program.to_account_info(),
                system_program::Transfer {
                    from: ctx.accounts.worker.to_account_info(),
                    to: ctx.accounts.vault.to_account_info(),
                },
            );
            system_program::transfer(cpi_ctx, task.worker_stake_required)?;
        }

        task.worker = ctx.accounts.worker.key();
        task.claimed_at = clock.unix_timestamp;
        task.status = TaskStatus::Claimed;

        emit!(TaskClaimed {
            task_id: task.task_id,
            worker: task.worker,
            claimed_at: task.claimed_at,
        });

        Ok(())
    }

    /// Agent commits completion state hash (SHA256 of computation payload).
    pub fn submit_work(ctx: Context<SubmitWork>, output_hash: [u8; 32]) -> Result<()> {
        let task = &mut ctx.accounts.task;
        require!(task.status == TaskStatus::Claimed, VibeError::InvalidTaskStatus);
        require!(task.worker == ctx.accounts.worker.key(), VibeError::Unauthorized);

        let clock = Clock::get()?;
        let deadline = task.claimed_at.checked_add(task.execution_window).ok_or(VibeError::MathOverflow)?;
        require!(clock.unix_timestamp <= deadline, VibeError::ExecutionWindowExpired);

        task.output_hash = output_hash;
        task.submitted_at = clock.unix_timestamp;
        task.status = TaskStatus::Submitted;

        emit!(WorkSubmitted {
            task_id: task.task_id,
            worker: task.worker,
            output_hash,
            submitted_at: task.submitted_at,
        });

        Ok(())
    }

    /// Settles task, distributes payment to worker, refunds stake, and pays protocol fee.
    pub fn settle_task(ctx: Context<SettleTask>) -> Result<()> {
        let task = &mut ctx.accounts.task;
        require!(task.status == TaskStatus::Submitted, VibeError::InvalidTaskStatus);

        let clock = Clock::get()?;
        let challenge_deadline = task
            .submitted_at
            .checked_add(task.challenge_window)
            .ok_or(VibeError::MathOverflow)?;

        // Must be called either after challenge window expired OR called directly by employer to accept immediately
        let caller = ctx.accounts.caller.key();
        let is_employer = caller == task.employer;
        let is_window_expired = clock.unix_timestamp >= challenge_deadline;

        require!(is_employer || is_window_expired, VibeError::ChallengeWindowActive);

        task.status = TaskStatus::Settled;

        let total_payout = task.reward_amount.checked_add(task.worker_stake_required).ok_or(VibeError::MathOverflow)?;
        let fee = (task.reward_amount as u128)
            .checked_mul(ctx.accounts.config.fee_basis_points as u128)
            .ok_or(VibeError::MathOverflow)?
            .checked_div(10000)
            .ok_or(VibeError::MathOverflow)? as u64;

        let worker_reward = total_payout.checked_sub(fee).ok_or(VibeError::MathOverflow)?;

        // Execute PDA transfers from Vault
        let employer_key = task.employer;
        let task_id = task.task_id;
        let vault_bump = task.vault_bump;
        let task_key = task.key();

        let vault_seeds: &[&[&[u8]]] = &[&[
            VAULT_SEED,
            task_key.as_ref(),
            &[vault_bump],
        ]];

        // 1. Fee transfer to treasury
        if fee > 0 {
            **ctx.accounts.vault.to_account_info().try_borrow_mut_lamports()? = ctx
                .accounts
                .vault
                .to_account_info()
                .lamports()
                .checked_sub(fee)
                .ok_or(VibeError::InsufficientVaultBalance)?;

            **ctx.accounts.treasury.to_account_info().try_borrow_mut_lamports()? = ctx
                .accounts
                .treasury
                .to_account_info()
                .lamports()
                .checked_add(fee)
                .ok_or(VibeError::MathOverflow)?;
        }

        // 2. Reward + Returned Stake transfer to worker
        **ctx.accounts.vault.to_account_info().try_borrow_mut_lamports()? = ctx
            .accounts
            .vault
            .to_account_info()
            .lamports()
            .checked_sub(worker_reward)
            .ok_or(VibeError::InsufficientVaultBalance)?;

        **ctx.accounts.worker.to_account_info().try_borrow_mut_lamports()? = ctx
            .accounts
            .worker
            .to_account_info()
            .lamports()
            .checked_add(worker_reward)
            .ok_or(VibeError::MathOverflow)?;

        ctx.accounts.config.total_tasks_settled = ctx
            .accounts
            .config
            .total_tasks_settled
            .checked_add(1)
            .unwrap();

        emit!(TaskSettled {
            task_id,
            worker: task.worker,
            payout: worker_reward,
            fee_paid: fee,
        });

        Ok(())
    }

    /// Employer or Arbiter challenges execution during the active challenge window.
    pub fn dispute_task(ctx: Context<DisputeTask>) -> Result<()> {
        let task = &mut ctx.accounts.task;
        require!(task.status == TaskStatus::Submitted, VibeError::InvalidTaskStatus);

        let clock = Clock::get()?;
        let challenge_deadline = task
            .submitted_at
            .checked_add(task.challenge_window)
            .ok_or(VibeError::MathOverflow)?;
        require!(clock.unix_timestamp < challenge_deadline, VibeError::ChallengeWindowExpired);

        task.status = TaskStatus::Disputed;

        emit!(TaskDisputed {
            task_id: task.task_id,
            disputer: ctx.accounts.authority.key(),
            timestamp: clock.unix_timestamp,
        });

        Ok(())
    }

    /// Cancels an unclaimed expired task or resolves an abandoned claim.
    pub fn cancel_task(ctx: Context<CancelTask>) -> Result<()> {
        let task = &mut ctx.accounts.task;
        let clock = Clock::get()?;

        let can_cancel = match task.status {
            TaskStatus::Created => true,
            TaskStatus::Claimed => {
                let deadline = task.claimed_at.checked_add(task.execution_window).ok_or(VibeError::MathOverflow)?;
                clock.unix_timestamp > deadline
            }
            _ => false,
        };

        require!(can_cancel, VibeError::TaskCannotBeCancelled);

        let vault_balance = ctx.accounts.vault.to_account_info().lamports();
        require!(vault_balance > 0, VibeError::InsufficientVaultBalance);

        // Refund all vault lamports to employer (including any forfeited stake if abandoned during claim)
        **ctx.accounts.vault.to_account_info().try_borrow_mut_lamports()? = 0;
        **ctx.accounts.employer.to_account_info().try_borrow_mut_lamports()? = ctx
            .accounts
            .employer
            .to_account_info()
            .lamports()
            .checked_add(vault_balance)
            .ok_or(VibeError::MathOverflow)?;

        task.status = TaskStatus::Cancelled;

        emit!(TaskCancelled {
            task_id: task.task_id,
            refund_amount: vault_balance,
        });

        Ok(())
    }
}

// ---------------------------------------------------------------------------
// ACCOUNT STRUCTURES & CONTEXTS
// ---------------------------------------------------------------------------

#[account]
#[derive(Default)]
pub struct ProtocolConfig {
    pub authority: Pubkey,        // 32
    pub treasury: Pubkey,         // 32
    pub fee_basis_points: u16,    // 2
    pub bump: u8,                 // 1
    pub total_tasks_created: u64, // 8
    pub total_tasks_settled: u64, // 8
}

impl ProtocolConfig {
    pub const LEN: usize = 8 + 32 + 32 + 2 + 1 + 8 + 8;
}

#[account]
pub struct TaskEscrow {
    pub employer: Pubkey,            // 32
    pub worker: Pubkey,              // 32
    pub task_id: [u8; 16],           // 16
    pub reward_amount: u64,          // 8
    pub worker_stake_required: u64,  // 8
    pub claimed_at: i64,             // 8
    pub submitted_at: i64,           // 8
    pub execution_window: i64,       // 8
    pub challenge_window: i64,       // 8
    pub output_hash: [u8; 32],       // 32
    pub status: TaskStatus,          // 1
    pub bump: u8,                    // 1
    pub vault_bump: u8,              // 1
}

impl TaskEscrow {
    pub const LEN: usize = 8 + 32 + 32 + 16 + 8 + 8 + 8 + 8 + 8 + 8 + 32 + 1 + 1 + 1;
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, PartialEq, Eq)]
pub enum TaskStatus {
    Created,
    Claimed,
    Submitted,
    Settled,
    Disputed,
    Cancelled,
}

#[derive(Accounts)]
pub struct InitializeProtocol<'info> {
    #[account(
        init,
        payer = authority,
        space = ProtocolConfig::LEN,
        seeds = [CONFIG_SEED],
        bump
    )]
    pub config: Account<'info, ProtocolConfig>,
    #[account(mut)]
    pub authority: Signer<'info>,
    /// CHECK: Protocol fee treasury recipient
    pub treasury: UncheckedAccount<'info>,
    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
#[instruction(task_id: [u8; 16])]
pub struct CreateTask<'info> {
    #[account(
        init,
        payer = employer,
        space = TaskEscrow::LEN,
        seeds = [TASK_SEED, employer.key().as_ref(), task_id.as_ref()],
        bump
    )]
    pub task: Account<'info, TaskEscrow>,
    #[account(
        mut,
        seeds = [VAULT_SEED, task.key().as_ref()],
        bump
    )]
    pub vault: SystemAccount<'info>,
    #[account(mut, seeds = [CONFIG_SEED], bump = config.bump)]
    pub config: Account<'info, ProtocolConfig>,
    #[account(mut)]
    pub employer: Signer<'info>,
    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
pub struct ClaimTask<'info> {
    #[account(
        mut,
        seeds = [TASK_SEED, task.employer.as_ref(), task.task_id.as_ref()],
        bump = task.bump
    )]
    pub task: Account<'info, TaskEscrow>,
    #[account(
        mut,
        seeds = [VAULT_SEED, task.key().as_ref()],
        bump = task.vault_bump
    )]
    pub vault: SystemAccount<'info>,
    #[account(mut)]
    pub worker: Signer<'info>,
    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
pub struct SubmitWork<'info> {
    #[account(
        mut,
        seeds = [TASK_SEED, task.employer.as_ref(), task.task_id.as_ref()],
        bump = task.bump
    )]
    pub task: Account<'info, TaskEscrow>,
    pub worker: Signer<'info>,
}

#[derive(Accounts)]
pub struct SettleTask<'info> {
    #[account(
        mut,
        seeds = [TASK_SEED, task.employer.as_ref(), task.task_id.as_ref()],
        bump = task.bump
    )]
    pub task: Account<'info, TaskEscrow>,
    #[account(
        mut,
        seeds = [VAULT_SEED, task.key().as_ref()],
        bump = task.vault_bump
    )]
    pub vault: SystemAccount<'info>,
    #[account(mut, seeds = [CONFIG_SEED], bump = config.bump)]
    pub config: Account<'info, ProtocolConfig>,
    /// CHECK: Validated against task.worker
    #[account(mut, address = task.worker)]
    pub worker: UncheckedAccount<'info>,
    /// CHECK: Validated against config.treasury
    #[account(mut, address = config.treasury)]
    pub treasury: UncheckedAccount<'info>,
    pub caller: Signer<'info>,
    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
pub struct DisputeTask<'info> {
    #[account(
        mut,
        seeds = [TASK_SEED, task.employer.as_ref(), task.task_id.as_ref()],
        bump = task.bump
    )]
    pub task: Account<'info, TaskEscrow>,
    #[account(seeds = [CONFIG_SEED], bump = config.bump)]
    pub config: Account<'info, ProtocolConfig>,
    #[account(
        constraint = authority.key() == task.employer || authority.key() == config.authority @ VibeError::Unauthorized
    )]
    pub authority: Signer<'info>,
}

#[derive(Accounts)]
pub struct CancelTask<'info> {
    #[account(
        mut,
        seeds = [TASK_SEED, task.employer.as_ref(), task.task_id.as_ref()],
        bump = task.bump
    )]
    pub task: Account<'info, TaskEscrow>,
    #[account(
        mut,
        seeds = [VAULT_SEED, task.key().as_ref()],
        bump = task.vault_bump
    )]
    pub vault: SystemAccount<'info>,
    #[account(mut, address = task.employer @ VibeError::Unauthorized)]
    pub employer: Signer<'info>,
    pub system_program: Program<'info, System>,
}

// ---------------------------------------------------------------------------
// EVENTS & ERRORS
// ---------------------------------------------------------------------------

#[event]
pub struct ProtocolInitialized {
    pub authority: Pubkey,
    pub treasury: Pubkey,
    pub fee_basis_points: u16,
}

#[event]
pub struct TaskCreated {
    pub task_id: [u8; 16],
    pub employer: Pubkey,
    pub reward_amount: u64,
    pub worker_stake_required: u64,
}

#[event]
pub struct TaskClaimed {
    pub task_id: [u8; 16],
    pub worker: Pubkey,
    pub claimed_at: i64,
}

#[event]
pub struct WorkSubmitted {
    pub task_id: [u8; 16],
    pub worker: Pubkey,
    pub output_hash: [u8; 32],
    pub submitted_at: i64,
}

#[event]
pub struct TaskSettled {
    pub task_id: [u8; 16],
    pub worker: Pubkey,
    pub payout: u64,
    pub fee_paid: u64,
}

#[event]
pub struct TaskDisputed {
    pub task_id: [u8; 16],
    pub disputer: Pubkey,
    pub timestamp: i64,
}

#[event]
pub struct TaskCancelled {
    pub task_id: [u8; 16],
    pub refund_amount: u64,
}

#[error_code]
pub enum VibeError {
    #[msg("Fee basis points cannot exceed 1000 (10%)")]
    FeeExceedsMaximum,
    #[msg("Reward amount must be greater than zero")]
    InvalidRewardAmount,
    #[msg("Window duration is too short to be viable")]
    WindowTooShort,
    #[msg("Invalid task status for this operation")]
    InvalidTaskStatus,
    #[msg("Unauthorized signer for this instruction")]
    Unauthorized,
    #[msg("Execution window has expired")]
    ExecutionWindowExpired,
    #[msg("Challenge window is still active")]
    ChallengeWindowActive,
    #[msg("Challenge window has expired")]
    ChallengeWindowExpired,
    #[msg("Arithmetic overflow detected")]
    MathOverflow,
    #[msg("Insufficient balance in task vault")]
    InsufficientVaultBalance,
    #[msg("Task cannot be cancelled under current conditions")]
    TaskCannotBeCancelled,
}