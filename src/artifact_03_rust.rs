use anchor_lang::prelude::*;
use anchor_lang::system_program::{transfer, Transfer};

declare_id!("V1beMesh11111111111111111111111111111111111");

pub const MAX_MILESTONES: u8 = 8;
pub const TASK_TIMEOUT_SECONDS: i64 = 86_400; // 24 hours
pub const PROTOCOL_FEE_BPS: u64 = 200;        // 2.00% fee

#[program]
pub mod vibe_mesh {
    use super::*;

    /// Initializes the global protocol configuration and platform fee recipient.
    pub fn initialize_protocol(
        ctx: Context<InitializeProtocol>,
        fee_basis_points: u16,
    ) -> Result<()> {
        require!(
            fee_basis_points <= 1000,
            VibeMeshError::ProtocolFeeTooHigh
        );
        let config = &mut ctx.accounts.config;
        config.admin = ctx.accounts.admin.key();
        config.fee_recipient = ctx.accounts.fee_recipient.key();
        config.fee_basis_points = fee_basis_points;
        config.bump = ctx.bumps.config;
        emit!(ProtocolInitializedEvent {
            admin: config.admin,
            fee_basis_points,
            timestamp: Clock::get()?.unix_timestamp,
        });
        Ok(())
    }

    /// Task Sponsor (e.g. Replit Developer) creates a bounty task with escrowed funds.
    pub fn create_task(
        ctx: Context<CreateTask>,
        task_id: [u8; 16],
        reward_lamports: u64,
        worker_stake_required: u64,
        milestones_count: u8,
        spec_hash: [u8; 32],
    ) -> Result<()> {
        require!(reward_lamports > 0, VibeMeshError::InvalidRewardAmount);
        require!(
            milestones_count > 0 && milestones_count <= MAX_MILESTONES,
            VibeMeshError::InvalidMilestoneCount
        );

        let task = &mut ctx.accounts.task;
        task.creator = ctx.accounts.creator.key();
        task.worker = Pubkey::default();
        task.task_id = task_id;
        task.reward_total = reward_lamports;
        task.reward_remaining = reward_lamports;
        task.worker_stake_required = worker_stake_required;
        task.worker_stake_locked = 0;
        task.milestones_count = milestones_count;
        task.milestones_completed = 0;
        task.spec_hash = spec_hash;
        task.execution_proof_hash = [0u8; 32];
        task.created_at = Clock::get()?.unix_timestamp;
        task.claimed_at = 0;
        task.deadline = 0;
        task.state = TaskState::Created;
        task.task_bump = ctx.bumps.task;
        task.vault_bump = ctx.bumps.escrow_vault;

        // Transfer funds from creator to the Escrow PDA
        let cpi_context = CpiContext::new(
            ctx.accounts.system_program.to_account_info(),
            Transfer {
                from: ctx.accounts.creator.to_account_info(),
                to: ctx.accounts.escrow_vault.to_account_info(),
            },
        );
        transfer(cpi_context, reward_lamports)?;

        emit!(TaskCreatedEvent {
            task: task.key(),
            creator: task.creator,
            task_id,
            reward_lamports,
            milestones_count,
            spec_hash,
            timestamp: task.created_at,
        });

        Ok(())
    }

    /// Autonomous AI Worker claims the bounty task and deposits required stake collateral.
    pub fn claim_task(ctx: Context<ClaimTask>, lease_duration: i64) -> Result<()> {
        let clock = Clock::get()?;
        let task = &mut ctx.accounts.task;

        require!(
            task.state == TaskState::Created,
            VibeMeshError::TaskNotAvailable
        );
        require!(
            lease_duration > 0 && lease_duration <= TASK_TIMEOUT_SECONDS,
            VibeMeshError::InvalidLeaseDuration
        );

        // Lock worker collateral if required
        if task.worker_stake_required > 0 {
            let cpi_context = CpiContext::new(
                ctx.accounts.system_program.to_account_info(),
                Transfer {
                    from: ctx.accounts.worker.to_account_info(),
                    to: ctx.accounts.escrow_vault.to_account_info(),
                },
            );
            transfer(cpi_context, task.worker_stake_required)?;
            task.worker_stake_locked = task.worker_stake_required;
        }

        task.worker = ctx.accounts.worker.key();
        task.claimed_at = clock.unix_timestamp;
        task.deadline = clock.unix_timestamp.checked_add(lease_duration).ok_or(VibeMeshError::MathOverflow)?;
        task.state = TaskState::Claimed;

        emit!(TaskClaimedEvent {
            task: task.key(),
            worker: task.worker,
            deadline: task.deadline,
            timestamp: clock.unix_timestamp,
        });

        Ok(())
    }

    /// Autonomous Worker submits milestone deliverables (e.g., passing commit hash or Merkle root).
    pub fn submit_milestone(
        ctx: Context<SubmitMilestone>,
        milestone_index: u8,
        proof_hash: [u8; 32],
    ) -> Result<()> {
        let task = &mut ctx.accounts.task;

        require!(task.state == TaskState::Claimed, VibeMeshError::InvalidTaskState);
        require_keys_eq!(ctx.accounts.worker.key(), task.worker, VibeMeshError::UnauthorizedWorker);
        require!(milestone_index == task.milestones_completed + 1, VibeMeshError::InvalidMilestoneOrder);
        require!(milestone_index <= task.milestones_count, VibeMeshError::MilestoneExceedsTotal);

        let clock = Clock::get()?;
        require!(clock.unix_timestamp <= task.deadline, VibeMeshError::TaskExpired);

        task.execution_proof_hash = proof_hash;

        emit!(MilestoneSubmittedEvent {
            task: task.key(),
            worker: task.worker,
            milestone_index,
            proof_hash,
            timestamp: clock.unix_timestamp,
        });

        Ok(())
    }

    /// Verifies milestone and triggers streaming disbursement to the worker.
    /// Can be executed by the task creator, or permissionlessly after 2 hours (keeper optimistic settlement).
    pub fn verify_and_settle(ctx: Context<VerifyAndSettle>) -> Result<()> {
        let clock = Clock::get()?;
        let task = &mut ctx.accounts.task;
        let config = &ctx.accounts.config;

        require!(
            task.state == TaskState::Claimed || task.state == TaskState::PartiallySettled,
            VibeMeshError::InvalidTaskState
        );

        let caller = ctx.accounts.signer.key();
        let is_creator = caller == task.creator;
        let is_optimistic_settlement = clock.unix_timestamp > task.claimed_at + 7200; // 2 hour grace

        require!(
            is_creator || is_optimistic_settlement,
            VibeMeshError::VerificationPrematureOrUnauthorized
        );

        task.milestones_completed = task.milestones_completed.checked_add(1).ok_or(VibeMeshError::MathOverflow)?;

        // Calculate milestone portion
        let milestone_payout = task.reward_total
            .checked_div(task.milestones_count as u64)
            .ok_or(VibeMeshError::MathOverflow)?;

        let fee_amount = milestone_payout
            .checked_mul(config.fee_basis_points as u64)
            .ok_or(VibeMeshError::MathOverflow)?
            .checked_div(10000)
            .ok_or(VibeMeshError::MathOverflow)?;

        let net_worker_payout = milestone_payout.checked_sub(fee_amount).ok_or(VibeMeshError::MathOverflow)?;

        let task_key = task.key();
        let vault_seeds = &[
            b"escrow_vault",
            task_key.as_ref(),
            &[task.vault_bump],
        ];
        let signer_seeds = &[&vault_seeds[..]];

        // 1. Disburse protocol fee
        if fee_amount > 0 {
            **ctx.accounts.escrow_vault.try_borrow_mut_lamports()? = ctx
                .accounts
                .escrow_vault
                .lamports()
                .checked_sub(fee_amount)
                .ok_or(VibeMeshError::MathOverflow)?;

            **ctx.accounts.fee_recipient.try_borrow_mut_lamports()? = ctx
                .accounts
                .fee_recipient
                .lamports()
                .checked_add(fee_amount)
                .ok_or(VibeMeshError::MathOverflow)?;
        }

        // 2. Disburse net worker payout
        **ctx.accounts.escrow_vault.try_borrow_mut_lamports()? = ctx
            .accounts
            .escrow_vault
            .lamports()
            .checked_sub(net_worker_payout)
            .ok_or(VibeMeshError::MathOverflow)?;

        **ctx.accounts.worker.try_borrow_mut_lamports()? = ctx
            .accounts
            .worker
            .lamports()
            .checked_add(net_worker_payout)
            .ok_or(VibeMeshError::MathOverflow)?;

        task.reward_remaining = task.reward_remaining.checked_sub(milestone_payout).ok_or(VibeMeshError::MathOverflow)?;

        // If all milestones completed, return worker stake and finalize
        if task.milestones_completed == task.milestones_count {
            task.state = TaskState::Settled;
            if task.worker_stake_locked > 0 {
                let stake = task.worker_stake_locked;
                **ctx.accounts.escrow_vault.try_borrow_mut_lamports()? = ctx
                    .accounts
                    .escrow_vault
                    .lamports()
                    .checked_sub(stake)
                    .ok_or(VibeMeshError::MathOverflow)?;

                **ctx.accounts.worker.try_borrow_mut_lamports()? = ctx
                    .accounts
                    .worker
                    .lamports()
                    .checked_add(stake)
                    .ok_or(VibeMeshError::MathOverflow)?;

                task.worker_stake_locked = 0;
            }
        } else {
            task.state = TaskState::PartiallySettled;
        }

        emit!(MilestoneSettledEvent {
            task: task.key(),
            worker: task.worker,
            milestone_index: task.milestones_completed,
            payout_lamports: net_worker_payout,
            fee_lamports: fee_amount,
            is_final: task.state == TaskState::Settled,
        });

        Ok(())
    }

    /// Sponsor or authorized verifier disputes an invalid output; locks escrow for arbitration.
    pub fn dispute_task(ctx: Context<DisputeTask>, reason_hash: [u8; 32]) -> Result<()> {
        let task = &mut ctx.accounts.task;
        require!(
            task.state == TaskState::Claimed || task.state == TaskState::PartiallySettled,
            VibeMeshError::InvalidTaskState
        );
        require_keys_eq!(ctx.accounts.creator.key(), task.creator, VibeMeshError::UnauthorizedCreator);

        task.state = TaskState::Disputed;

        emit!(TaskDisputedEvent {
            task: task.key(),
            creator: task.creator,
            worker: task.worker,
            reason_hash,
            timestamp: Clock::get()?.unix_timestamp,
        });

        Ok(())
    }

    /// Allows task cancellation if unclaimed or expired past the deadline without progress.
    pub fn cancel_task(ctx: Context<CancelTask>) -> Result<()> {
        let clock = Clock::get()?;
        let task = &mut ctx.accounts.task;

        let can_cancel = (task.state == TaskState::Created)
            || (task.state == TaskState::Claimed && clock.unix_timestamp > task.deadline && task.milestones_completed == 0);

        require!(can_cancel, VibeMeshError::CannotCancelTask);
        require_keys_eq!(ctx.accounts.creator.key(), task.creator, VibeMeshError::UnauthorizedCreator);

        let refund_amount = task.reward_remaining;
        let worker_stake = task.worker_stake_locked;

        let task_key = task.key();
        let _vault_seeds = &[
            b"escrow_vault",
            task_key.as_ref(),
            &[task.vault_bump],
        ];

        // Refund creator remaining bounty
        if refund_amount > 0 {
            **ctx.accounts.escrow_vault.try_borrow_mut_lamports()? = ctx
                .accounts
                .escrow_vault
                .lamports()
                .checked_sub(refund_amount)
                .ok_or(VibeMeshError::MathOverflow)?;

            **ctx.accounts.creator.try_borrow_mut_lamports()? = ctx
                .accounts
                .creator
                .lamports()
                .checked_add(refund_amount)
                .ok_or(VibeMeshError::MathOverflow)?;
            task.reward_remaining = 0;
        }

        // Return worker stake if deadline expired
        if worker_stake > 0 {
            **ctx.accounts.escrow_vault.try_borrow_mut_lamports()? = ctx
                .accounts
                .escrow_vault
                .lamports()
                .checked_sub(worker_stake)
                .ok_or(VibeMeshError::MathOverflow)?;

            **ctx.accounts.worker.try_borrow_mut_lamports()? = ctx
                .accounts
                .worker
                .lamports()
                .checked_add(worker_stake)
                .ok_or(VibeMeshError::MathOverflow)?;
            task.worker_stake_locked = 0;
        }

        task.state = TaskState::Cancelled;

        emit!(TaskCancelledEvent {
            task: task.key(),
            refunded_lamports: refund_amount,
            timestamp: clock.unix_timestamp,
        });

        Ok(())
    }
}

// -----------------------------------------------------------------------------
// ACCOUNT VALIDATION STRUCTS
// -----------------------------------------------------------------------------

#[derive(Accounts)]
pub struct InitializeProtocol<'info> {
    #[account(
        init,
        payer = admin,
        space = 8 + MeshConfig::INIT_SPACE,
        seeds = [b"mesh_config"],
        bump
    )]
    pub config: Account<'info, MeshConfig>,
    #[account(mut)]
    pub admin: Signer<'info>,
    /// CHECK: Safe protocol fee collector
    pub fee_recipient: AccountInfo<'info>,
    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
#[instruction(task_id: [u8; 16])]
pub struct CreateTask<'info> {
    #[account(
        init,
        payer = creator,
        space = 8 + TaskAccount::INIT_SPACE,
        seeds = [b"task", creator.key().as_ref(), task_id.as_ref()],
        bump
    )]
    pub task: Account<'info, TaskAccount>,
    /// CHECK: PDA acting as the vault holding task SOL/SPL funds
    #[account(
        mut,
        seeds = [b"escrow_vault", task.key().as_ref()],
        bump
    )]
    pub escrow_vault: AccountInfo<'info>,
    #[account(mut)]
    pub creator: Signer<'info>,
    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
pub struct ClaimTask<'info> {
    #[account(
        mut,
        seeds = [b"task", task.creator.as_ref(), task.task_id.as_ref()],
        bump = task.task_bump
    )]
    pub task: Account<'info, TaskAccount>,
    /// CHECK: PDA Escrow Vault
    #[account(
        mut,
        seeds = [b"escrow_vault", task.key().as_ref()],
        bump = task.vault_bump
    )]
    pub escrow_vault: AccountInfo<'info>,
    #[account(mut)]
    pub worker: Signer<'info>,
    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
pub struct SubmitMilestone<'info> {
    #[account(
        mut,
        seeds = [b"task", task.creator.as_ref(), task.task_id.as_ref()],
        bump = task.task_bump
    )]
    pub task: Account<'info, TaskAccount>,
    pub worker: Signer<'info>,
}

#[derive(Accounts)]
pub struct VerifyAndSettle<'info> {
    #[account(
        seeds = [b"mesh_config"],
        bump = config.bump
    )]
    pub config: Account<'info, MeshConfig>,
    #[account(
        mut,
        seeds = [b"task", task.creator.as_ref(), task.task_id.as_ref()],
        bump = task.task_bump
    )]
    pub task: Account<'info, TaskAccount>,
    /// CHECK: Escrow PDA
    #[account(
        mut,
        seeds = [b"escrow_vault", task.key().as_ref()],
        bump = task.vault_bump
    )]
    pub escrow_vault: AccountInfo<'info>,
    /// CHECK: Worker account to receive release
    #[account(mut, address = task.worker)]
    pub worker: AccountInfo<'info>,
    /// CHECK: Fee recipient verified against config
    #[account(mut, address = config.fee_recipient)]
    pub fee_recipient: AccountInfo<'info>,
    pub signer: Signer<'info>,
    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
pub struct DisputeTask<'info> {
    #[account(
        mut,
        seeds = [b"task", task.creator.as_ref(), task.task_id.as_ref()],
        bump = task.task_bump
    )]
    pub task: Account<'info, TaskAccount>,
    pub creator: Signer<'info>,
}

#[derive(Accounts)]
pub struct CancelTask<'info> {
    #[account(
        mut,
        seeds = [b"task", task.creator.as_ref(), task.task_id.as_ref()],
        bump = task.task_bump
    )]
    pub task: Account<'info, TaskAccount>,
    /// CHECK: Escrow PDA
    #[account(
        mut,
        seeds = [b"escrow_vault", task.key().as_ref()],
        bump = task.vault_bump
    )]
    pub escrow_vault: AccountInfo<'info>,
    #[account(mut)]
    pub creator: Signer<'info>,
    /// CHECK: Worker account receiving collateral back if applicable
    #[account(mut)]
    pub worker: AccountInfo<'info>,
    pub system_program: Program<'info, System>,
}

// -----------------------------------------------------------------------------
// STATE STRUCTURES
// -----------------------------------------------------------------------------

#[account]
#[derive(InitSpace)]
pub struct MeshConfig {
    pub admin: Pubkey,            // 32
    pub fee_recipient: Pubkey,    // 32
    pub fee_basis_points: u16,    // 2
    pub bump: u8,                 // 1
}

#[account]
#[derive(InitSpace)]
pub struct TaskAccount {
    pub creator: Pubkey,               // 32
    pub worker: Pubkey,                // 32
    pub task_id: [u8; 16],             // 16
    pub reward_total: u64,             // 8
    pub reward_remaining: u64,         // 8
    pub worker_stake_required: u64,    // 8
    pub worker_stake_locked: u64,      // 8
    pub spec_hash: [u8; 32],           // 32
    pub execution_proof_hash: [u8; 32],// 32
    pub created_at: i64,               // 8
    pub claimed_at: i64,               // 8
    pub deadline: i64,                 // 8
    pub milestones_count: u8,          // 1
    pub milestones_completed: u8,      // 1
    pub state: TaskState,              // 1
    pub task_bump: u8,                 // 1
    pub vault_bump: u8,                // 1
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, PartialEq, Eq, InitSpace)]
pub enum TaskState {
    Created,
    Claimed,
    PartiallySettled,
    Settled,
    Disputed,
    Cancelled,
}

// -----------------------------------------------------------------------------
// EVENTS & ERRORS
// -----------------------------------------------------------------------------

#[event]
pub struct ProtocolInitializedEvent {
    pub admin: Pubkey,
    pub fee_basis_points: u16,
    pub timestamp: i64,
}

#[event]
pub struct TaskCreatedEvent {
    pub task: Pubkey,
    pub creator: Pubkey,
    pub task_id: [u8; 16],
    pub reward_lamports: u64,
    pub milestones_count: u8,
    pub spec_hash: [u8; 32],
    pub timestamp: i64,
}

#[event]
pub struct TaskClaimedEvent {
    pub task: Pubkey,
    pub worker: Pubkey,
    pub deadline: i64,
    pub timestamp: i64,
}

#[event]
pub struct MilestoneSubmittedEvent {
    pub task: Pubkey,
    pub worker: Pubkey,
    pub milestone_index: u8,
    pub proof_hash: [u8; 32],
    pub timestamp: i64,
}

#[event]
pub struct MilestoneSettledEvent {
    pub task: Pubkey,
    pub worker: Pubkey,
    pub milestone_index: u8,
    pub payout_lamports: u64,
    pub fee_lamports: u64,
    pub is_final: bool,
}

#[event]
pub struct TaskDisputedEvent {
    pub task: Pubkey,
    pub creator: Pubkey,
    pub worker: Pubkey,
    pub reason_hash: [u8; 32],
    pub timestamp: i64,
}

#[event]
pub struct TaskCancelledEvent {
    pub task: Pubkey,
    pub refunded_lamports: u64,
    pub timestamp: i64,
}

#[error_code]
pub enum VibeMeshError {
    #[msg("Protocol fee exceeds maximum allowed 10.00% (1000 bps)")]
    ProtocolFeeTooHigh,
    #[msg("Reward amount must be greater than zero")]
    InvalidRewardAmount,
    #[msg("Milestone count must be between 1 and 8")]
    InvalidMilestoneCount,
    #[msg("Task is not in Created state and cannot be claimed")]
    TaskNotAvailable,
    #[msg("Lease duration must be > 0 and <= 24 hours")]
    InvalidLeaseDuration,
    #[msg("Task state invalid for requested transition")]
    InvalidTaskState,
    #[msg("Signer is not authorized to act as worker")]
    UnauthorizedWorker,
    #[msg("Milestone index does not match next expected sequence")]
    InvalidMilestoneOrder,
    #[msg("Milestone index exceeds maximum declared milestones")]
    MilestoneExceedsTotal,
    #[msg("Task lease duration deadline has been exceeded")]
    TaskExpired,
    #[msg("Settlement is premature: must be signed by creator or wait for dispute window")]
    VerificationPrematureOrUnauthorized,
    #[msg("Signer is not authorized to act as task creator")]
    UnauthorizedCreator,
    #[msg("Task cannot be cancelled in its current state")]
    CannotCancelTask,
    #[msg("Arithmetic overflow occurred")]
    MathOverflow,
}