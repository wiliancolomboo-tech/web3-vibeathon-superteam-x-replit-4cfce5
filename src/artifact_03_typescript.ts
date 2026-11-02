import * as anchor from "@coral-xyz/anchor";
import { Program } from "@coral-xyz/anchor";
import { Vibemesh } from "../target/types/vibemesh";
import {
  Keypair,
  LAMPORTS_PER_SOL,
  PublicKey,
  SystemProgram,
} from "@solana/web3.js";
import { expect } from "chai";
import * as crypto from "crypto";

describe("VibeMesh: Autonomous Agent Escrow QA Test Matrix", () => {
  const provider = anchor.AnchorProvider.env();
  anchor.setProvider(provider);

  const program = anchor.workspace.Vibemesh as Program<Vibemesh>;

  const protocolAdmin = Keypair.generate();
  const treasuryAccount = Keypair.generate();
  const employerAgent = Keypair.generate();
  const workerAgent = Keypair.generate();
  const rogueAgent = Keypair.generate();

  let configPda: PublicKey;
  let configBump: number;

  const feeBps = 250; // 2.5% protocol cut

  before(async () => {
    // Fund test participants
    const airdropSigners = [
      protocolAdmin,
      employerAgent,
      workerAgent,
      rogueAgent,
    ];
    for (const signer of airdropSigners) {
      const sig = await provider.connection.requestAirdrop(
        signer.publicKey,
        10 * LAMPORTS_PER_SOL
      );
      const latestBlockhash = await provider.connection.getLatestBlockhash();
      await provider.connection.confirmTransaction({
        signature: sig,
        ...latestBlockhash,
      });
    }

    [configPda, configBump] = PublicKey.findProgramAddressSync(
      [Buffer.from("config")],
      program.programId
    );
  });

  it("1. Initializes protocol registry and global config", async () => {
    await program.methods
      .initializeProtocol(feeBps)
      .accounts({
        config: configPda,
        authority: protocolAdmin.publicKey,
        treasury: treasuryAccount.publicKey,
        systemProgram: SystemProgram.programId,
      })
      .signers([protocolAdmin])
      .rpc();

    const configAccount = await program.account.protocolConfig.fetch(configPda);
    expect(configAccount.authority.toBase58()).to.equal(
      protocolAdmin.publicKey.toBase58()
    );
    expect(configAccount.feeBasisPoints).to.equal(feeBps);
    expect(configAccount.totalTasksCreated.toNumber()).to.equal(0);
  });

  it("2. Creates a task escrow with isolated PDA vault", async () => {
    const taskId = Array.from(crypto.randomBytes(16));
    const rewardAmount = new anchor.BN(1 * LAMPORTS_PER_SOL);
    const workerStake = new anchor.BN(0.2 * LAMPORTS_PER_SOL);
    const execWindow = new anchor.BN(60); // 60 seconds
    const challengeWindow = new anchor.BN(2); // 2 seconds for test velocity

    const [taskPda] = PublicKey.findProgramAddressSync(
      [
        Buffer.from("task"),
        employerAgent.publicKey.toBuffer(),
        Buffer.from(taskId),
      ],
      program.programId
    );

    const [vaultPda] = PublicKey.findProgramAddressSync(
      [Buffer.from("vault"), taskPda.toBuffer()],
      program.programId
    );

    await program.methods
      .createTask(
        taskId,
        rewardAmount,
        workerStake,
        execWindow,
        challengeWindow
      )
      .accounts({
        task: taskPda,
        vault: vaultPda,
        config: configPda,
        employer: employerAgent.publicKey,
        systemProgram: SystemProgram.programId,
      })
      .signers([employerAgent])
      .rpc();

    const taskAccount = await program.account.taskEscrow.fetch(taskPda);
    expect(taskAccount.employer.toBase58()).to.equal(
      employerAgent.publicKey.toBase58()
    );
    expect(taskAccount.rewardAmount.eq(rewardAmount)).to.be.true;

    const vaultLamports = await provider.connection.getBalance(vaultPda);
    expect(vaultLamports).to.equal(rewardAmount.toNumber());
  });

  it("3. Worker Agent claims task and deposits security stake", async () => {
    const taskId = Array.from(crypto.randomBytes(16));
    const rewardAmount = new anchor.BN(0.5 * LAMPORTS_PER_SOL);
    const workerStake = new anchor.BN(0.1 * LAMPORTS_PER_SOL);

    const [taskPda] = PublicKey.findProgramAddressSync(
      [
        Buffer.from("task"),
        employerAgent.publicKey.toBuffer(),
        Buffer.from(taskId),
      ],
      program.programId
    );
    const [vaultPda] = PublicKey.findProgramAddressSync(
      [Buffer.from("vault"), taskPda.toBuffer()],
      program.programId
    );

    await program.methods
      .createTask(
        taskId,
        rewardAmount,
        workerStake,
        new anchor.BN(120),
        new anchor.BN(2)
      )
      .accounts({
        task: taskPda,
        vault: vaultPda,
        config: configPda,
        employer: employerAgent.publicKey,
        systemProgram: SystemProgram.programId,
      })
      .signers([employerAgent])
      .rpc();

    // Claim
    await program.methods
      .claimTask()
      .accounts({
        task: taskPda,
        vault: vaultPda,
        worker: workerAgent.publicKey,
        systemProgram: SystemProgram.programId,
      })
      .signers([workerAgent])
      .rpc();

    const taskAccount = await program.account.taskEscrow.fetch(taskPda);
    expect(taskAccount.worker.toBase58()).to.equal(
      workerAgent.publicKey.toBase58()
    );

    const vaultBalance = await provider.connection.getBalance(vaultPda);
    expect(vaultBalance).to.equal(
      rewardAmount.add(workerStake).toNumber()
    );
  });

  it("4. Prevents rogue agent from submitting work on claimed task", async () => {
    const taskId = Array.from(crypto.randomBytes(16));
    const [taskPda] = PublicKey.findProgramAddressSync(
      [
        Buffer.from("task"),
        employerAgent.publicKey.toBuffer(),
        Buffer.from(taskId),
      ],
      program.programId
    );
    const [vaultPda] = PublicKey.findProgramAddressSync(
      [Buffer.from("vault"), taskPda.toBuffer()],
      program.programId
    );

    await program.methods
      .createTask(
        taskId,
        new anchor.BN(0.2 * LAMPORTS_PER_SOL),
        new anchor.BN(0),
        new anchor.BN(60),
        new anchor.BN(2)
      )
      .accounts({
        task: taskPda,
        vault: vaultPda,
        config: configPda,
        employer: employerAgent.publicKey,
        systemProgram: SystemProgram.programId,
      })
      .signers([employerAgent])
      .rpc();

    await program.methods
      .claimTask()
      .accounts({
        task: taskPda,
        vault: vaultPda,
        worker: workerAgent.publicKey,
        systemProgram: SystemProgram.programId,
      })
      .signers([workerAgent])
      .rpc();

    const dummyHash = Array.from(crypto.createHash("sha256").update("res").digest());

    try {
      await program.methods
        .submitWork(dummyHash)
        .accounts({
          task: taskPda,
          worker: rogueAgent.publicKey,
        })
        .signers([rogueAgent])
        .rpc();
      expect.fail("Rogue submission should have reverted");
    } catch (err: any) {
      expect(err.error.errorCode.code).to.equal("Unauthorized");
    }
  });

  it("5. Happy path: Submit work, pass challenge window, settle payout and fee", async () => {
    const taskId = Array.from(crypto.randomBytes(16));
    const rewardAmount = new anchor.BN(1 * LAMPORTS_PER_SOL);
    const workerStake = new anchor.BN(0.1 * LAMPORTS_PER_SOL);
    const challengeWindow = new anchor.BN(2); // 2s challenge window

    const [taskPda] = PublicKey.findProgramAddressSync(
      [
        Buffer.from("task"),
        employerAgent.publicKey.toBuffer(),
        Buffer.from(taskId),
      ],
      program.programId
    );
    const [vaultPda] = PublicKey.findProgramAddressSync(
      [Buffer.from("vault"), taskPda.toBuffer()],
      program.programId
    );

    // 1. Create
    await program.methods
      .createTask(taskId, rewardAmount, workerStake, new anchor.BN(60), challengeWindow)
      .accounts({
        task: taskPda,
        vault: vaultPda,
        config: configPda,
        employer: employerAgent.publicKey,
        systemProgram: SystemProgram.programId,
      })
      .signers([employerAgent])
      .rpc();

    // 2. Claim
    await program.methods
      .claimTask()
      .accounts({
        task: taskPda,
        vault: vaultPda,
        worker: workerAgent.publicKey,
        systemProgram: SystemProgram.programId,
      })
      .signers([workerAgent])
      .rpc();

    // 3. Submit
    const artifactHash = Array.from(
      crypto.createHash("sha256").update("execution_result_artifact_v1").digest()
    );
    await program.methods
      .submitWork(artifactHash)
      .accounts({
        task: taskPda,
        worker: workerAgent.publicKey,
      })
      .signers([workerAgent])
      .rpc();

    // 4. Assert early settlement fails during challenge window
    try {
      await program.methods
        .settleTask()
        .accounts({
          task: taskPda,
          vault: vaultPda,
          config: configPda,
          worker: workerAgent.publicKey,
          treasury: treasuryAccount.publicKey,
          caller: workerAgent.publicKey,
          systemProgram: SystemProgram.programId,
        })
        .signers([workerAgent])
        .rpc();
      expect.fail("Should have failed while challenge window was active");
    } catch (err: any) {
      expect(err.error.errorCode.code).to.equal("ChallengeWindowActive");
    }

    // Wait out the challenge window
    await new Promise((resolve) => setTimeout(resolve, 3000));

    const workerPreBalance = await provider.connection.getBalance(
      workerAgent.publicKey
    );
    const treasuryPreBalance = await provider.connection.getBalance(
      treasuryAccount.publicKey
    );

    // 5. Successful settlement
    await program.methods
      .settleTask()
      .accounts({
        task: taskPda,
        vault: vaultPda,
        config: configPda,
        worker: workerAgent.publicKey,
        treasury: treasuryAccount.publicKey,
        caller: workerAgent.publicKey,
        systemProgram: SystemProgram.programId,
      })
      .signers([workerAgent])
      .rpc();

    const workerPostBalance = await provider.connection.getBalance(
      workerAgent.publicKey
    );
    const treasuryPostBalance = await provider.connection.getBalance(
      treasuryAccount.publicKey
    );

    const expectedFee = rewardAmount.toNumber() * (feeBps / 10000); // 0.025 SOL
    const expectedWorkerNet =
      rewardAmount.toNumber() + workerStake.toNumber() - expectedFee;

    expect(treasuryPostBalance - treasuryPreBalance).to.equal(expectedFee);
    // Worker post-balance delta accounts for payout minus tx fees
    expect(workerPostBalance - workerPreBalance).to.be.closeTo(
      expectedWorkerNet,
      5000000 // 0.005 SOL margin for transaction fees
    );
  });
});