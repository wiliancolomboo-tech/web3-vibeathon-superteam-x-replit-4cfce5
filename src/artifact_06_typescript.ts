import * as anchor from "@coral-xyz/anchor";
import { Program } from "@coral-xyz/anchor";
import { Vibestream } from "../target/types/vibestream";
import {
  Keypair,
  LAMPORTS_PER_SOL,
  PublicKey,
  SystemProgram,
  SYSVAR_INSTRUCTIONS_PUBKEY,
  Ed25519Program,
  Transaction,
} from "@solana/web3.js";
import { expect } from "chai";
import * as crypto from "crypto";

describe("VibeStream Protocol Integration Matrix", () => {
  const provider = anchor.AnchorProvider.env();
  anchor.setProvider(provider);

  const program = anchor.workspace.Vibestream as Program<Vibestream>;

  const admin = Keypair.generate();
  const feeRecipient = Keypair.generate();
  const creator = Keypair.generate();
  const agentAuthority = Keypair.generate();
  const oracleSigner = Keypair.generate();

  let configPda: PublicKey;
  let agentPda: PublicKey;
  const taskId = new anchor.BN(42069);
  let taskPda: PublicKey;
  let vaultPda: PublicKey;

  const rawSecretOutput = "replit://workspace/run/artifact-verified-build-v1";
  const expectedHash = crypto.createHash("sha256").update(rawSecretOutput).digest();

  before(async () => {
    // AirDrop Sol to participants
    const airdropSigners = [admin, creator, agentAuthority];
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

    [configPda] = PublicKey.findProgramAddressSync(
      [Buffer.from("config")],
      program.programId
    );

    [agentPda] = PublicKey.findProgramAddressSync(
      [Buffer.from("agent"), agentAuthority.publicKey.toBuffer()],
      program.programId
    );

    [taskPda] = PublicKey.findProgramAddressSync(
      [
        Buffer.from("task"),
        creator.publicKey.toBuffer(),
        taskId.toArrayLike(Buffer, "le", 8),
      ],
      program.programId
    );

    [vaultPda] = PublicKey.findProgramAddressSync(
      [
        Buffer.from("vault"),
        creator.publicKey.toBuffer(),
        taskId.toArrayLike(Buffer, "le", 8),
      ],
      program.programId
    );
  });

  it("1. Initializes Global Protocol State with fee configuration", async () => {
    await program.methods
      .initializeProtocol(feeRecipient.publicKey)
      .accounts({
        config: configPda,
        admin: admin.publicKey,
        systemProgram: SystemProgram.programId,
      })
      .signers([admin])
      .rpc();

    const configAccount = await program.account.globalConfig.fetch(configPda);
    expect(configAccount.admin.toBase58()).to.equal(admin.publicKey.toBase58());
    expect(configAccount.feeRecipient.toBase58()).to.equal(feeRecipient.publicKey.toBase58());
    expect(configAccount.totalTasksCompleted.toNumber()).to.equal(0);
  });

  it("2. Registers an Autonomous Replit Agent Profile with Collateral Deposit", async () => {
    const stakeAmount = new anchor.BN(0.1 * LAMPORTS_PER_SOL);
    const handle = "replit_agent_codex_9";

    await program.methods
      .registerAgent(handle, stakeAmount)
      .accounts({
        agent: agentPda,
        authority: agentAuthority.publicKey,
        systemProgram: SystemProgram.programId,
      })
      .signers([agentAuthority])
      .rpc();

    const agentAccount = await program.account.agentProfile.fetch(agentPda);
    expect(agentAccount.owner.toBase58()).to.equal(agentAuthority.publicKey.toBase58());
    expect(agentAccount.handle).to.equal(handle);
    expect(agentAccount.stakedCollateral.toNumber()).to.equal(stakeAmount.toNumber());
    expect(agentAccount.reputationScore).to.equal(100);
  });

  it("3. Creates and funds a Task Escrow with Native SOL", async () => {
    const bounty = new anchor.BN(1 * LAMPORTS_PER_SOL);
    const deadline = new anchor.BN(Math.floor(Date.now() / 1000) + 3600); // 1 hr

    await program.methods
      .initializeTask(taskId, bounty, deadline, Array.from(expectedHash))
      .accounts({
        task: taskPda,
        vault: vaultPda,
        creator: creator.publicKey,
        systemProgram: SystemProgram.programId,
      })
      .signers([creator])
      .rpc();

    const taskAccount = await program.account.taskEscrow.fetch(taskPda);
    expect(taskAccount.creator.toBase58()).to.equal(creator.publicKey.toBase58());
    expect(taskAccount.bountyAmount.toNumber()).to.equal(bounty.toNumber());

    const vaultBalance = await provider.connection.getBalance(vaultPda);
    expect(vaultBalance).to.equal(bounty.toNumber());
  });

  it("4. Submits computed solution hash from autonomous agent", async () => {
    await program.methods
      .submitSolution(Array.from(expectedHash))
      .accounts({
        task: taskPda,
        agent: agentPda,
        agentSigner: agentAuthority,
      })
      .signers([agentAuthority])
      .rpc();

    const taskAccount = await program.account.taskEscrow.fetch(taskPda);
    expect(JSON.stringify(taskAccount.status)).to.include("pendingVerification");
    expect(Buffer.from(taskAccount.actualOutputHash).toString("hex")).to.equal(
      expectedHash.toString("hex")
    );
  });

  it("5. Settles Escrow via Ed25519 signature introspection verification", async () => {
    // Construct signed attestation payload
    const message = Buffer.from(`TaskVerified:${taskId.toString()}`);
    const ed25519Instruction = Ed25519Program.createInstructionWithPrivateKey({
      privateKey: oracleSigner.secretKey,
      message,
    });

    const initialAgentBal = await provider.connection.getBalance(agentAuthority.publicKey);
    const initialFeeRecipientBal = await provider.connection.getBalance(feeRecipient.publicKey);

    // Atomic TX with Ed25519 verification as instruction index 0 and verify_and_settle as index 1
    const settleIx = await program.methods
      .verifyAndSettle()
      .accounts({
        config: configPda,
        task: taskPda,
        vault: vaultPda,
        agent: agentPda,
        agentAuthority: agentAuthority.publicKey,
        feeRecipient: feeRecipient.publicKey,
        instructionSysvar: SYSVAR_INSTRUCTIONS_PUBKEY,
        systemProgram: SystemProgram.programId,
      })
      .instruction();

    const tx = new Transaction().add(ed25519Instruction).add(settleIx);
    await provider.sendAndConfirm(tx, [agentAuthority]);

    // Validate balances after split (1.5% fee vs 98.5% payout)
    const taskBounty = 1 * LAMPORTS_PER_SOL;
    const expectedFee = (taskBounty * 150) / 10000;
    const expectedAgentPayout = taskBounty - expectedFee;

    const finalFeeBal = await provider.connection.getBalance(feeRecipient.publicKey);
    expect(finalFeeBal - initialFeeRecipientBal).to.equal(expectedFee);

    const taskAccount = await program.account.taskEscrow.fetch(taskPda);
    expect(JSON.stringify(taskAccount.status)).to.include("completed");

    const agentProfile = await program.account.agentProfile.fetch(agentPda);
    expect(agentProfile.tasksCompleted.toNumber()).to.equal(1);
    expect(agentProfile.reputationScore).to.equal(105);
  });

  it("6. Rejects settlement if Ed25519 Pre-Instruction is missing (Anti-tamper guard)", async () => {
    const expiredTaskId = new anchor.BN(9999);
    const [tamperTaskPda] = PublicKey.findProgramAddressSync(
      [Buffer.from("task"), creator.publicKey.toBuffer(), expiredTaskId.toArrayLike(Buffer, "le", 8)],
      program.programId
    );
    const [tamperVaultPda] = PublicKey.findProgramAddressSync(
      [Buffer.from("vault"), creator.publicKey.toBuffer(), expiredTaskId.toArrayLike(Buffer, "le", 8)],
      program.programId
    );

    const bounty = new anchor.BN(0.05 * LAMPORTS_PER_SOL);
    const deadline = new anchor.BN(Math.floor(Date.now() / 1000) + 100);

    await program.methods
      .initializeTask(expiredTaskId, bounty, deadline, Array.from(expectedHash))
      .accounts({
        task: tamperTaskPda,
        vault: tamperVaultPda,
        creator: creator.publicKey,
        systemProgram: SystemProgram.programId,
      })
      .signers([creator])
      .rpc();

    await program.methods
      .submitSolution(Array.from(expectedHash))
      .accounts({
        task: tamperTaskPda,
        agent: agentPda,
        agentSigner: agentAuthority,
      })
      .signers([agentAuthority])
      .rpc();

    try {
      // Intentionally omit the Ed25519 instruction to trigger MissingEd25519Preflight
      await program.methods
        .verifyAndSettle()
        .accounts({
          config: configPda,
          task: tamperTaskPda,
          vault: tamperVaultPda,
          agent: agentPda,
          agentAuthority: agentAuthority.publicKey,
          feeRecipient: feeRecipient.publicKey,
          instructionSysvar: SYSVAR_INSTRUCTIONS_PUBKEY,
          systemProgram: SystemProgram.programId,
        })
        .signers([agentAuthority])
        .rpc();
      expect.fail("Should have failed due to missing Ed25519 preflight instruction");
    } catch (err: any) {
      expect(err.toString()).to.contain("MissingEd25519Preflight");
    }
  });
});