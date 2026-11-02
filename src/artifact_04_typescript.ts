import * as anchor from "@coral-xyz/anchor";
import { Program } from "@coral-xyz/anchor";
import { VibeMesh } from "../target/types/vibe_mesh";
import { PublicKey, Keypair, SystemProgram, LAMPORTS_PER_SOL } from "@solana/web3.js";
import { expect } from "chai";
import * as crypto from "crypto";

describe("vibe_mesh Protocol Verification Matrix", () => {
  const provider = anchor.AnchorProvider.env();
  anchor.setProvider(provider);

  const program = anchor.workspace.VibeMesh as Program<VibeMesh>;

  const admin = Keypair.generate();
  const feeRecipient = Keypair.generate();
  const creator = Keypair.generate();
  const workerAgent = Keypair.generate();
  const maliciousActor = Keypair.generate();

  let configPda: PublicKey;
  let taskId: number[];
  let taskPda: PublicKey;
  let escrowVaultPda: PublicKey;

  before(async () => {
    // Fund test wallets
    const airdropSigners = [admin, creator, workerAgent, maliciousActor];
    for (const signer of airdropSigners) {
      const sig = await provider.connection.requestAirdrop(signer.publicKey, 10 * LAMPORTS_PER_SOL);
      const latestBlockhash = await provider.connection.getLatestBlockhash();
      await provider.connection.confirmTransaction({
        signature: sig,
        ...latestBlockhash,
      });
    }

    [configPda] = PublicKey.findProgramAddressSync(
      [Buffer.from("mesh_config")],
      program.programId
    );
  });

  it("1. Initializes the MeshConfig PDA correctly", async () => {
    const feeBps = 200; // 2.00%
    await program.methods
      .initializeProtocol(feeBps)
      .accountsPartial({
        config: configPda,
        admin: admin.publicKey,
        feeRecipient: feeRecipient.publicKey,
        systemProgram: SystemProgram.programId,
      })
      .signers([admin])
      .rpc();

    const configAccount = await program.account.meshConfig.fetch(configPda);
    expect(configAccount.admin.toBase58()).to.equal(admin.publicKey.toBase58());
    expect(configAccount.feeRecipient.toBase58()).to.equal(feeRecipient.publicKey.toBase58());
    expect(configAccount.feeBasisPoints).to.equal(feeBps);
  });

  it("2. Task Creator initializes a multi-milestone task and escrows SOL", async () => {
    taskId = Array.from(crypto.randomBytes(16));
    const specHash = Array.from(crypto.createHash("sha256").update("replit-task-ast-parse").digest());
    const rewardLamports = new anchor.BN(2 * LAMPORTS_PER_SOL);
    const workerStake = new anchor.BN(0.5 * LAMPORTS_PER_SOL);
    const milestonesCount = 2;

    [taskPda] = PublicKey.findProgramAddressSync(
      [Buffer.from("task"), creator.publicKey.toBuffer(), Buffer.from(taskId)],
      program.programId
    );

    [escrowVaultPda] = PublicKey.findProgramAddressSync(
      [Buffer.from("escrow_vault"), taskPda.toBuffer()],
      program.programId
    );

    const vaultPreBalance = await provider.connection.getBalance(escrowVaultPda).catch(() => 0);

    await program.methods
      .createTask(taskId, rewardLamports, workerStake, milestonesCount, specHash)
      .accountsPartial({
        task: taskPda,
        escrowVault: escrowVaultPda,
        creator: creator.publicKey,
        systemProgram: SystemProgram.programId,
      })
      .signers([creator])
      .rpc();

    const taskAccount = await program.account.taskAccount.fetch(taskPda);
    expect(taskAccount.creator.toBase58()).to.equal(creator.publicKey.toBase58());
    expect(taskAccount.rewardTotal.toNumber()).to.equal(rewardLamports.toNumber());
    expect(taskAccount.milestonesCount).to.equal(milestonesCount);
    expect(taskAccount.state).to.deep.equal({ created: {} });

    const vaultPostBalance = await provider.connection.getBalance(escrowVaultPda);
    expect(vaultPostBalance - vaultPreBalance).to.equal(rewardLamports.toNumber());
  });

  it("3. Autonomous Agent claims the task with required stake deposit", async () => {
    const leaseDuration = new anchor.BN(3600); // 1 hour lease

    await program.methods
      .claimTask(leaseDuration)
      .accountsPartial({
        task: taskPda,
        escrowVault: escrowVaultPda,
        worker: workerAgent.publicKey,
        systemProgram: SystemProgram.programId,
      })
      .signers([workerAgent])
      .rpc();

    const taskAccount = await program.account.taskAccount.fetch(taskPda);
    expect(taskAccount.worker.toBase58()).to.equal(workerAgent.publicKey.toBase58());
    expect(taskAccount.state).to.deep.equal({ claimed: {} });
    expect(taskAccount.workerStakeLocked.toNumber()).to.equal(0.5 * LAMPORTS_PER_SOL);
  });

  it("4. Rejects out-of-order milestone submission", async () => {
    const fakeProof = Array.from(crypto.createHash("sha256").update("invalid-order").digest());
    try {
      await program.methods
        .submitMilestone(2, fakeProof) // Attempt milestone 2 before milestone 1
        .accountsPartial({
          task: taskPda,
          worker: workerAgent.publicKey,
        })
        .signers([workerAgent])
        .rpc();
      expect.fail("Expected out-of-order submission to fail");
    } catch (err: any) {
      expect(err.error.errorCode.code).to.equal("InvalidMilestoneOrder");
    }
  });

  it("5. Worker successfully submits milestone 1 proof hash", async () => {
    const milestoneProof = Array.from(crypto.createHash("sha256").update("milestone-1-ast-complete").digest());

    await program.methods
      .submitMilestone(1, milestoneProof)
      .accountsPartial({
        task: taskPda,
        worker: workerAgent.publicKey,
      })
      .signers([workerAgent])
      .rpc();

    const taskAccount = await program.account.taskAccount.fetch(taskPda);
    expect(taskAccount.executionProofHash).to.deep.equal(milestoneProof);
  });

  it("6. Creator verifies milestone 1 and releases streamed micro-settlement", async () => {
    const workerPreBalance = await provider.connection.getBalance(workerAgent.publicKey);
    const feeRecipientPreBalance = await provider.connection.getBalance(feeRecipient.publicKey);

    await program.methods
      .verifyAndSettle()
      .accountsPartial({
        config: configPda,
        task: taskPda,
        escrowVault: escrowVaultPda,
        worker: workerAgent.publicKey,
        feeRecipient: feeRecipient.publicKey,
        signer: creator.publicKey,
        systemProgram: SystemProgram.programId,
      })
      .signers([creator])
      .rpc();

    const taskAccount = await program.account.taskAccount.fetch(taskPda);
    expect(taskAccount.milestonesCompleted).to.equal(1);
    expect(taskAccount.state).to.deep.equal({ partiallySettled: {} });

    // Payout calculation: 2 SOL / 2 milestones = 1 SOL. 2% fee = 0.02 SOL. Worker net = 0.98 SOL.
    const workerPostBalance = await provider.connection.getBalance(workerAgent.publicKey);
    const feeRecipientPostBalance = await provider.connection.getBalance(feeRecipient.publicKey);

    expect(workerPostBalance - workerPreBalance).to.equal(0.98 * LAMPORTS_PER_SOL);
    expect(feeRecipientPostBalance - feeRecipientPreBalance).to.equal(0.02 * LAMPORTS_PER_SOL);
  });

  it("7. Worker submits milestone 2 and reaches full settlement with stake refund", async () => {
    const milestone2Proof = Array.from(crypto.createHash("sha256").update("milestone-2-fuzzing-passed").digest());

    await program.methods
      .submitMilestone(2, milestone2Proof)
      .accountsPartial({
        task: taskPda,
        worker: workerAgent.publicKey,
      })
      .signers([workerAgent])
      .rpc();

    const workerPreBalance = await provider.connection.getBalance(workerAgent.publicKey);

    await program.methods
      .verifyAndSettle()
      .accountsPartial({
        config: configPda,
        task: taskPda,
        escrowVault: escrowVaultPda,
        worker: workerAgent.publicKey,
        feeRecipient: feeRecipient.publicKey,
        signer: creator.publicKey,
        systemProgram: SystemProgram.programId,
      })
      .signers([creator])
      .rpc();

    const taskAccount = await program.account.taskAccount.fetch(taskPda);
    expect(taskAccount.milestonesCompleted).to.equal(2);
    expect(taskAccount.state).to.deep.equal({ settled: {} });
    expect(taskAccount.rewardRemaining.toNumber()).to.equal(0);
    expect(taskAccount.workerStakeLocked.toNumber()).to.equal(0);

    // Worker receives 0.98 SOL + 0.5 SOL returned stake = 1.48 SOL
    const workerPostBalance = await provider.connection.getBalance(workerAgent.publicKey);
    expect(workerPostBalance - workerPreBalance).to.equal(1.48 * LAMPORTS_PER_SOL);
  });

  it("8. Malicious actor cannot dispute or tamper with settled tasks", async () => {
    const dummyHash = Array.from(crypto.randomBytes(32));
    try {
      await program.methods
        .disputeTask(dummyHash)
        .accountsPartial({
          task: taskPda,
          creator: maliciousActor.publicKey,
        })
        .signers([maliciousActor])
        .rpc();
      expect.fail("Expected dispute by unauthorized attacker to fail");
    } catch (err: any) {
      expect(err.error.errorCode.code).to.equal("InvalidTaskState");
    }
  });
});