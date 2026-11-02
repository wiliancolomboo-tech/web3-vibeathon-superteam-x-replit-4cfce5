import * as anchor from "@coral-xyz/anchor";
import { Program } from "@coral-xyz/anchor";
import { VibeEscrow } from "../target/types/vibe_escrow";
import {
  Keypair,
  LAMPORTS_PER_SOL,
  PublicKey,
  SystemProgram,
} from "@solana/web3.js";
import { assert, expect } from "chai";
import * as crypto from "crypto";

describe("vibe_escrow test suite", () => {
  const provider = anchor.AnchorProvider.env();
  anchor.setProvider(provider);

  const program = anchor.workspace.VibeEscrow as Program<VibeEscrow>;

  const admin = Keypair.generate();
  const treasury = Keypair.generate();
  const creator = Keypair.generate();
  const worker = Keypair.generate();
  const oracle = Keypair.generate();

  const [globalConfigPDA] = PublicKey.findProgramAddressSync(
    [Buffer.from("global_config")],
    program.programId
  );

  const taskId = new anchor.BN(42069);
  const rewardAmount = new anchor.BN(1 * LAMPORTS_PER_SOL);
  const taskTimeoutSeconds = new anchor.BN(300); // 5 mins
  const rawDigest = crypto.createHash("sha256").update("replit-task-solution-v1").digest();
  const expectedCommitHash = Array.from(rawDigest);

  const [taskEscrowPDA] = PublicKey.findProgramAddressSync(
    [
      Buffer.from("task_escrow"),
      creator.publicKey.toBuffer(),
      taskId.toArrayLike(Buffer, "le", 8),
    ],
    program.programId
  );

  const [escrowVaultPDA] = PublicKey.findProgramAddressSync(
    [Buffer.from("escrow_vault"), taskEscrowPDA.toBuffer()],
    program.programId
  );

  before(async () => {
    // Fund test actors
    const airdropSigners = [admin, creator, worker, oracle];
    for (const signer of airdropSigners) {
      const sig = await provider.connection.requestAirdrop(
        signer.publicKey,
        5 * LAMPORTS_PER_SOL
      );
      const latestBlockhash = await provider.connection.getLatestBlockhash();
      await provider.connection.confirmTransaction({
        signature: sig,
        ...latestBlockhash,
      });
    }
  });

  it("1. Initializes Global Config (2.5% fee)", async () => {
    const feeBps = 250; // 2.5%
    await program.methods
      .initializeGlobalConfig(feeBps)
      .accounts({
        admin: admin.publicKey,
        treasury: treasury.publicKey,
        globalConfig: globalConfigPDA,
        systemProgram: SystemProgram.programId,
      })
      .signers([admin])
      .rpc();

    const configAccount = await program.account.globalConfig.fetch(
      globalConfigPDA
    );
    expect(configAccount.admin.toBase58()).to.equal(admin.publicKey.toBase58());
    expect(configAccount.treasury.toBase58()).to.equal(
      treasury.publicKey.toBase58()
    );
    expect(configAccount.feeBasisPoints).to.equal(250);
  });

  it("2. Spawns and funds a Task Escrow from Replit Creator", async () => {
    const metadata = "https://replit.com/@agent/task-42069.json";

    await program.methods
      .createTask(
        taskId,
        rewardAmount,
        taskTimeoutSeconds,
        expectedCommitHash,
        metadata
      )
      .accounts({
        creator: creator.publicKey,
        verificationOracle: oracle.publicKey,
        taskEscrow: taskEscrowPDA,
        escrowVault: escrowVaultPDA,
        systemProgram: SystemProgram.programId,
      })
      .signers([creator])
      .rpc();

    const taskAccount = await program.account.taskEscrow.fetch(taskEscrowPDA);
    expect(taskAccount.creator.toBase58()).to.equal(creator.publicKey.toBase58());
    expect(taskAccount.rewardAmount.toNumber()).to.equal(rewardAmount.toNumber());
    expect(taskAccount.state).to.deep.equal({ open: {} });

    const vaultBalance = await provider.connection.getBalance(escrowVaultPDA);
    expect(vaultBalance).to.equal(rewardAmount.toNumber());
  });

  it("3. AI Worker Agent claims the Task", async () => {
    await program.methods
      .claimTask(taskId)
      .accounts({
        worker: worker.publicKey,
        taskEscrow: taskEscrowPDA,
      })
      .signers([worker])
      .rpc();

    const taskAccount = await program.account.taskEscrow.fetch(taskEscrowPDA);
    expect(taskAccount.assignedWorker.toBase58()).to.equal(
      worker.publicKey.toBase58()
    );
    expect(taskAccount.state).to.deep.equal({ claimed: {} });
  });

  it("4. Blocks settlement if worker passes invalid commit hash", async () => {
    const forgedHash = Array.from(
      crypto.createHash("sha256").update("malicious-pr-tampered").digest()
    );

    try {
      await program.methods
        .verifyAndSettle(taskId, forgedHash)
        .accounts({
          worker: worker.publicKey,
          verificationOracle: oracle.publicKey,
          globalConfig: globalConfigPDA,
          taskEscrow: taskEscrowPDA,
          escrowVault: escrowVaultPDA,
          treasury: treasury.publicKey,
          systemProgram: SystemProgram.programId,
        })
        .signers([worker, oracle])
        .rpc();
      assert.fail("Settlement should have failed due to commit mismatch");
    } catch (err: any) {
      expect(err.error.errorCode.code).to.equal("CommitHashMismatch");
    }
  });

  it("5. Settles Escrow: Worker verified by Oracle, distributes payout & platform fee", async () => {
    const workerBalanceBefore = await provider.connection.getBalance(
      worker.publicKey
    );
    const treasuryBalanceBefore = await provider.connection.getBalance(
      treasury.publicKey
    );

    await program.methods
      .verifyAndSettle(taskId, expectedCommitHash)
      .accounts({
        worker: worker.publicKey,
        verificationOracle: oracle.publicKey,
        globalConfig: globalConfigPDA,
        taskEscrow: taskEscrowPDA,
        escrowVault: escrowVaultPDA,
        treasury: treasury.publicKey,
        systemProgram: SystemProgram.programId,
      })
      .signers([worker, oracle])
      .rpc();

    const taskAccount = await program.account.taskEscrow.fetch(taskEscrowPDA);
    expect(taskAccount.state).to.deep.equal({ completed: {} });

    const workerBalanceAfter = await provider.connection.getBalance(
      worker.publicKey
    );
    const treasuryBalanceAfter = await provider.connection.getBalance(
      treasury.publicKey
    );

    const expectedFee = (1.0 * 250) / 10000 * LAMPORTS_PER_SOL; // 0.025 SOL
    const expectedWorkerPayout = 1 * LAMPORTS_PER_SOL - expectedFee; // 0.975 SOL

    expect(treasuryBalanceAfter - treasuryBalanceBefore).to.equal(expectedFee);
    // Worker balance check incorporates small tx fee deduct
    expect(workerBalanceAfter).to.be.greaterThan(
      workerBalanceBefore + expectedWorkerPayout - 0.005 * LAMPORTS_PER_SOL
    );
  });
});