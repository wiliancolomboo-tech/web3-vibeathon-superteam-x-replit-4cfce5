use anchor_lang::prelude::*;
use anchor_lang::system_program::{transfer, Transfer};
use solana_program::sysvar::instructions as ix_sysvar;

declare_id!("V1beStream1111111111111111111111111111111111");

pub const DISCRIMINATOR_LENGTH: usize = 8;
pub const MAX_METADATA_LEN: usize = 64;
pub const PROTOCOL_FEE_BASIS_POINTS: u64 = 150; // 1.50%
pub const BPS_DENOMINATOR: u64 = 10_000;

#[program]
pub mod vibestream {
    use super::*;

    /// Initializes the global configuration containing the platform fee recipient.
    pub fn initialize_protocol(ctx: Context<InitializeProtocol>, fee_recipient: Pubkey) -> Result<()> {
        let config = &mut ctx.accounts.config;
        config.admin = ctx.accounts.admin.key();
        config.fee_recipient = fee_recipient;
        config.total_tasks_completed = 0;
        config.bump = ctx.bumps.config;
        emit!(ProtocolInitialized {
            admin: config.admin,
            fee_recipient,
        });
        Ok(())
    }

    /// Registers an autonomous Replit agent profile with staked security collateral.
    pub fn register_agent(
        ctx: Context<RegisterAgent>,
        agent_handle: String,
        stake_amount: u64,
    ) -> Result<()> {
        require!(
            agent_handle.as_bytes().len() <= MAX_METADATA_LEN,
            VibeStreamError::MetadataTooLong
        );
        require!(stake_amount >= 10_000_000, VibeStreamError::InsufficientCollateral); // min 0.01 SOL

        let agent = &mut ctx.accounts.agent;
        agent.owner = ctx.accounts.authority.key();
        agent.staked_collateral = stake_amount;
        agent.tasks_completed = 0;
        agent.reputation_score = 100; // Baseline initial reputation
        agent.handle = agent_handle;
        agent.bump = ctx.bumps.agent;

        // Transfer stake to agent PDA vault
        let cpi_ctx = CpiContext::new(
            ctx.accounts.system_program.to_account_info(),
            Transfer {
                from: ctx.accounts.authority.to_account_info(),
                to: agent.to_account_info(),
            },
        );
        transfer(cpi_ctx, stake_amount)?;

        emit!(AgentRegistered {
            agent_pubkey: agent.key(),
            authority: ctx.accounts.authority.key(),
            stake_amount,
        });
        Ok(())
    }

    /// Creates a bounty task escrow funded with native SOL.
    pub fn initialize_task(
        ctx: Context<InitializeTask>,
        task_id: u64,
        bounty_amount: u64,
        deadline_timestamp: i64,
        expected_output_hash: [u8; 32],
    ) -> Result<()> {
        let clock = Clock::get()?;
        require!(
            deadline_timestamp > clock.unix_timestamp,
            VibeStreamError::InvalidDeadline
        );
        require!(bounty_amount > 0, VibeStreamError::ZeroBountyAmount);

        let task = &mut ctx.accounts.task;
        task.creator = ctx.accounts.creator.key();
        task.task_id = task_id;
        task.bounty_amount = bounty_amount;
        task.deadline_timestamp = deadline_timestamp;
        task.expected_output_hash = expected_output_hash;
        task.actual_output_hash = [0u8; 32];
        task.assigned_agent = Pubkey::default();
        task.status = TaskStatus::Created;
        task.bump = ctx.bumps.task;
        task.vault_bump = ctx.bumps.vault;

        // Lock bounty into Vault PDA
        let cpi_ctx = CpiContext::new(
            ctx.accounts.system_program.to_account_info(),
            Transfer {
                from: ctx.accounts.creator.to_account_info(),
                to: ctx.accounts.vault.to_account_info(),
            },
        );
        transfer(cpi_ctx, bounty_amount)?;

        emit!(TaskCreated {
            task_id,
            creator: task.creator,
            bounty_amount,
            deadline: deadline_timestamp,
        });
        Ok(())
    }

    /// Autonomous agent claims and commits an execution solution hash.
    pub fn submit_solution(
        ctx: Context<SubmitSolution>,
        computed_output_hash: [u8; 32],
    ) -> Result<()> {
        let task = &mut ctx.accounts.task;
        let clock = Clock::get()?;

        require!(
            task.status == TaskStatus::Created,
            VibeStreamError::TaskNotOpen
        );
        require!(
            clock.unix_timestamp <= task.deadline_timestamp,
            VibeStreamError::TaskExpired
        );

        task.assigned_agent = ctx.accounts.agent.key();
        task.actual_output_hash = computed_output_hash;
        task.status = TaskStatus::PendingVerification;

        emit!(SolutionSubmitted {
            task_id: task.task_id,
            agent: task.assigned_agent,
            computed_output_hash,
        });
        Ok(())
    }

    /// Verifies task attestation using Ed25519 verification pre-flight instruction & settles funds.
    pub fn verify_and_settle(ctx: Context<VerifyAndSettle>) -> Result<()> {
        let task = &mut ctx.accounts.task;
        let config = &mut ctx.accounts.config;
        let agent = &mut ctx.accounts.agent;

        require!(
            task.status == TaskStatus::PendingVerification,
            VibeStreamError::InvalidTaskState
        );
        require!(
            task.assigned_agent == agent.key(),
            VibeStreamError::AgentMismatch
        );
        require!(
            task.actual_output_hash == task.expected_output_hash,
            VibeStreamError::OutputHashMismatch
        );

        // Security Guard: Validate Ed25519 instruction exists immediately prior in the transaction
        let ix_acc = &ctx.accounts.instruction_sysvar;
        let current_ix_index = ix_sysvar::load_current_index_checked(ix_acc)?;
        require!(current_ix_index > 0, VibeStreamError::MissingEd25519Preflight);

        let previous_ix = ix_sysvar::load_instruction_at_checked((current_ix_index - 1) as usize, ix_acc)?;
        // Verify previous instruction was sent to the native Ed25519 Program
        require!(
            previous_ix.program_id == solana_program::ed25519_program::ID,
            VibeStreamError::InvalidEd25519ProgramId
        );

        // Calculate platform protocol fee & worker net distribution
        let total_bounty = task.bounty_amount;
        let platform_fee = (total_bounty * PROTOCOL_FEE_BASIS_POINTS) / BPS_DENOMINATOR;
        let agent_payout = total_bounty
            .checked_sub(platform_fee)
            .ok_or(VibeStreamError::ArithmeticOverflow)?;

        // Vault PDA Signer Seeds
        let task_creator = task.creator;
        let task_id_bytes = task.task_id.to_le_bytes();
        let vault_bump = task.vault_bump;

        let vault_seeds: &[&[&[u8]]] = &[&[
            b"vault",
            task_creator.as_ref(),
            task_id_bytes.as_ref(),
            &[vault_bump],
        ]];

        // 1. Transfer platform fee to protocol fee recipient
        let transfer_fee_cpi = CpiContext::new_with_signer(
            ctx.accounts.system_program.to_account_info(),
            Transfer {
                from: ctx.accounts.vault.to_account_info(),
                to: ctx.accounts.fee_recipient.to_account_info(),
            },
            vault_seeds,
        );
        transfer(transfer_fee_cpi, platform_fee)?;

        // 2. Transfer net payout to agent worker authority
        let transfer_agent_cpi = CpiContext::new_with_signer(
            ctx.accounts.system_program.to_account_info(),
            Transfer {
                from: ctx.accounts.vault.to_account_info(),
                to: ctx.accounts.agent_authority.to_account_info(),
            },
            vault_seeds,
        );
        transfer(transfer_agent_cpi, agent_payout)?;

        // State progression
        task.status = TaskStatus::Completed;
        agent.tasks_completed = agent.tasks_completed.saturating_add(1);
        agent.reputation_score = agent.reputation_score.saturating_add(5);
        config.total_tasks_completed = config.total_tasks_completed.saturating_add(1);

        emit!(TaskSettled {
            task_id: task.task_id,
            agent: agent.key(),
            agent_payout,
            platform_fee,
        });

        Ok(())
    }

    /// Allows creator to clawback funds if the deadline passed without completion.
    pub fn cancel_expired_task(ctx: Context<CancelExpiredTask>) -> Result<()> {
        let task = &mut ctx.accounts.task;
        let clock = Clock::get()?;

        require!(
            clock.unix_timestamp > task.deadline_timestamp,
            VibeStreamError::TaskNotExpired
        );
        require!(
            task.status != TaskStatus::Completed,
            VibeStreamError::TaskAlreadyCompleted
        );

        let vault_balance = ctx.accounts.vault.to_account_info().lamports();
        require!(vault_balance > 0, VibeStreamError::VaultAlreadyEmpty);

        let task_creator = task.creator;
        let task_id_bytes = task.task_id.to_le_bytes();
        let vault_bump = task.vault_bump;

        let vault_seeds: &[&[&[u8]]] = &[&[
            b"vault",
            task_creator.as_ref(),
            task_id_bytes.as_ref(),
            &[vault_bump],
        ]];

        let refund_cpi = CpiContext::new_with_signer(
            ctx.accounts.system_program.to_account_info(),
            Transfer {
                from: ctx.accounts.vault.to_account_info(),
                to: ctx.accounts.creator.to_account_info(),
            },
            vault_seeds,
        );
        transfer(refund_cpi, vault_balance)?;

        task.status = TaskStatus::Cancelled;

        emit!(TaskCancelled {
            task_id: task.task_id,
            refund_amount: vault_balance,
        });
        Ok(())
    }
}

// -----------------------------------------------------------------------------
// ACCOUNT VALIDATION STRUCTS
// -----------------------------------------------------------------------------

#[derive(Accounts)]
pub fn initialize_protocol(ctx: Context<InitializeProtocol>) -> Result<()> {
    Ok(())
}

#[derive(Accounts)]
pub struct InitializeProtocol<'info> {
    #[account(
        init,
        payer = admin,
        space = DISCRIMINATOR_LENGTH + GlobalConfig::INIT_SPACE,
        seeds = [b"config"],
        bump
    )]
    pub config: Account<'info, GlobalConfig>,
    #[account(mut)]
    pub admin: Signer<'info>,
    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
#[instruction(agent_handle: String)]
pub struct RegisterAgent<'info> {
    #[account(
        init,
        payer = authority,
        space = DISCRIMINATOR_LENGTH + AgentProfile::INIT_SPACE,
        seeds = [b"agent", authority.key().as_ref()],
        bump
    )]
    pub agent: Account<'info, AgentProfile>,
    #[account(mut)]
    pub authority: Signer<'info>,
    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
#[instruction(task_id: u64)]
pub struct InitializeTask<'info> {
    #[account(
        init,
        payer = creator,
        space = DISCRIMINATOR_LENGTH + TaskEscrow::INIT_SPACE,
        seeds = [b"task", creator.key().as_ref(), &task_id.to_le_bytes()],
        bump
    )]
    pub task: Account<'info, TaskEscrow>,
    #[account(
        mut,
        seeds = [b"vault", creator.key().as_ref(), &task_id.to_le_bytes()],
        bump
    )]
    /// CHECK: Native lamports PDA vault owned by the VibeStream program
    pub vault: AccountInfo<'info>,
    #[account(mut)]
    pub creator: Signer<'info>,
    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
pub struct SubmitSolution<'info> {
    #[account(
        mut,
        seeds = [b"task", task.creator.as_ref(), &task.task_id.to_le_bytes()],
        bump = task.bump
    )]
    pub task: Account<'info, TaskEscrow>,
    #[account(
        seeds = [b"agent", agent.owner.as_ref()],
        bump = agent.bump
    )]
    pub agent: Account<'info, AgentProfile>,
    pub agent_signer: Signer<'info>,
}

#[derive(Accounts)]
pub struct VerifyAndSettle<'info> {
    #[account(
        mut,
        seeds = [b"config"],
        bump = config.bump
    )]
    pub config: Account<'info, GlobalConfig>,
    #[account(
        mut,
        seeds = [b"task", task.creator.as_ref(), &task.task_id.to_le_bytes()],
        bump = task.bump
    )]
    pub task: Account<'info, TaskEscrow>,
    #[account(
        mut,
        seeds = [b"vault", task.creator.as_ref(), &task.task_id.to_le_bytes()],
        bump = task.vault_bump
    )]
    /// CHECK: Checked via PDA seeds and mutable balance checks
    pub vault: AccountInfo<'info>,
    #[account(
        mut,
        seeds = [b"agent", agent.owner.as_ref()],
        bump = agent.bump
    )]
    pub agent: Account<'info, AgentProfile>,
    #[account(mut, address = agent.owner)]
    /// CHECK: Recipient worker authority validated against agent.owner
    pub agent_authority: AccountInfo<'info>,
    #[account(mut, address = config.fee_recipient)]
    /// CHECK: Protocol fee recipient validated against protocol configuration
    pub fee_recipient: AccountInfo<'info>,
    #[account(address = ix_sysvar::ID)]
    /// CHECK: Instruction sysvar address constraint enforced
    pub instruction_sysvar: AccountInfo<'info>,
    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
pub struct CancelExpiredTask<'info> {
    #[account(
        mut,
        seeds = [b"task", creator.key().as_ref(), &task.task_id.to_le_bytes()],
        bump = task.bump,
        has_one = creator @ VibeStreamError::UnauthorizedCreator
    )]
    pub task: Account<'info, TaskEscrow>,
    #[account(
        mut,
        seeds = [b"vault", creator.key().as_ref(), &task.task_id.to_le_bytes()],
        bump = task.vault_bump
    )]
    /// CHECK: Checked via seeds and bump
    pub vault: AccountInfo<'info>,
    #[account(mut)]
    pub creator: Signer<'info>,
    pub system_program: Program<'info, System>,
}

// -----------------------------------------------------------------------------
// STATE SCHEMAS
// -----------------------------------------------------------------------------

#[account]
#[derive(InitSpace)]
pub struct GlobalConfig {
    pub admin: Pubkey,
    pub fee_recipient: Pubkey,
    pub total_tasks_completed: u64,
    pub bump: u8,
}

#[account]
#[derive(InitSpace)]
pub struct AgentProfile {
    pub owner: Pubkey,
    pub staked_collateral: u64,
    pub tasks_completed: u64,
    pub reputation_score: u32,
    #[max_len(64)]
    pub handle: String,
    pub bump: u8,
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, PartialEq, Eq, InitSpace)]
pub enum TaskStatus {
    Created,
    PendingVerification,
    Completed,
    Cancelled,
}

#[account]
#[derive(InitSpace)]
pub struct TaskEscrow {
    pub creator: Pubkey,
    pub task_id: u64,
    pub bounty_amount: u64,
    pub deadline_timestamp: i64,
    pub expected_output_hash: [u8; 32],
    pub actual_output_hash: [u8; 32],
    pub assigned_agent: Pubkey,
    pub status: TaskStatus,
    pub bump: u8,
    pub vault_bump: u8,
}

// -----------------------------------------------------------------------------
// EVENTS & ERRORS
// -----------------------------------------------------------------------------

#[event]
pub struct ProtocolInitialized {
    pub admin: Pubkey,
    pub fee_recipient: Pubkey,
}

#[event]
pub struct AgentRegistered {
    pub agent_pubkey: Pubkey,
    pub authority: Pubkey,
    pub stake_amount: u64,
}

#[event]
pub struct TaskCreated {
    pub task_id: u64,
    pub creator: Pubkey,
    pub bounty_amount: u64,
    pub deadline: i64,
}

#[event]
pub struct SolutionSubmitted {
    pub task_id: u64,
    pub agent: Pubkey,
    pub computed_output_hash: [u8; 32],
}

#[event]
pub struct TaskSettled {
    pub task_id: u64,
    pub agent: Pubkey,
    pub agent_payout: u64,
    pub platform_fee: u64,
}

#[event]
pub struct TaskCancelled {
    pub task_id: u64,
    pub refund_amount: u64,
}

#[error_code]
pub enum VibeStreamError {
    #[msg("Metadata string exceeds allocated bounds (max 64 bytes)")]
    MetadataTooLong,
    #[msg("Agent collateral deposit is below the mandatory threshold")]
    InsufficientCollateral,
    #[msg("Provided deadline must be in the future")]
    InvalidDeadline,
    #[msg("Task bounty escrow must be greater than zero")]
    ZeroBountyAmount,
    #[msg("Task is not open for submission")]
    TaskNotOpen,
    #[msg("Task submission deadline has already elapsed")]
    TaskExpired,
    #[msg("Task state invalid for operation")]
    InvalidTaskState,
    #[msg("Assigned agent does not match submitted agent profile")]
    AgentMismatch,
    #[msg("Computed output hash does not match expected output commitment")]
    OutputHashMismatch,
    #[msg("Preflight instruction missing Ed25519 signature verification")]
    MissingEd25519Preflight,
    #[msg("Instruction preceding verify_and_settle was not the Ed25519 native program")]
    InvalidEd25519ProgramId,
    #[msg("Safe arithmetic underflow or overflow encountered")]
    ArithmeticOverflow,
    #[msg("Cannot cancel task before expiration timestamp")]
    TaskNotExpired,
    #[msg("Task has already been completed")]
    TaskAlreadyCompleted,
    #[msg("Task vault balance is zero")]
    VaultAlreadyEmpty,
    #[msg("Signer is not authorized creator for this task")]
    UnauthorizedCreator,
}