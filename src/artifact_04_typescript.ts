import * as anchor from "@coral-xyz/anchor";
import { Program } from "@coral-xyz/anchor";
import { VibeVault } from "../target/types/vibe_vault";
import {
  Keypair,
  LAMPORTS_PER_SOL,
  PublicKey,
  SystemProgram,
} from "@solana/web3.js";
import { expect } from "chai";

describe("vibe_vault e2e test matrix", () => {
  const provider = anchor.AnchorProvider.env();
  anchor.setProvider(provider);

  const program = anchor.workspace.VibeVault as Program<VibeVault>;

  const owner = Keypair.generate();
  const agent = Keypair.generate();
  const unauthorizedAgent = Keypair.generate();
  const recipient = Keypair.generate();
  const keeper = Keypair.generate();

  const vaultId = new anchor.BN(42069);
  const maxRate = new anchor.BN(10_000_000); // 0.01 SOL/sec
  const streamRate = new anchor.BN(1_000_000); // 0.001 SOL/sec

  let vaultPda: PublicKey;
  let treasuryPda: PublicKey;
  let streamPda: PublicKey;

  before(async () => {
    // Fund test keypairs
    const airdropSigners = [owner, agent, unauthorizedAgent, keeper];
    for (const signer of airdropSigners) {
      const sig = await provider.connection.requestAirdrop(
        signer.publicKey,
        5 * LAMPORTS_PER_SOL
      );
      await provider.connection.confirmTransaction(sig);
    }

    [vaultPda] = PublicKey.findProgramAddressSync(
      [
        Buffer.from("vault"),
        owner.publicKey.toBuffer(),
        vaultId.toArrayLike(Buffer, "le", 8),
      ],
      program.programId
    );

    [treasuryPda] = PublicKey.findProgramAddressSync(
      [Buffer.from("treasury"), vaultPda.toBuffer()],
      program.programId
    );

    [streamPda] = PublicKey.findProgramAddressSync(
      [Buffer.from("stream"), vaultPda.toBuffer(), recipient.publicKey.toBuffer()],
      program.programId
    );
  });

  it("Initializes VibeVault successfully", async () => {
    const tx = await program.methods
      .initializeVault(vaultId, maxRate)
      .accounts({
        vault: vaultPda,
        treasury: treasuryPda,
        initialAgent: agent.publicKey,
        owner: owner.publicKey,
        systemProgram: SystemProgram.programId,
      })
      .signers([owner])
      .rpc();

    const vaultAccount = await program.account.vaultState.fetch(vaultPda);
    expect(vaultAccount.owner.toBase58()).to.equal(owner.publicKey.toBase58());
    expect(vaultAccount.agentKey.toBase58()).to.equal(agent.publicKey.toBase58());
    expect(vaultAccount.maxRatePerSecond.toNumber()).to.equal(maxRate.toNumber());
  });

  it("Deposits native SOL into Vault Treasury PDA", async () => {
    const depositAmount = new anchor.BN(3 * LAMPORTS_PER_SOL);

    await program.methods
      .depositFunds(depositAmount)
      .accounts({
        vault: vaultPda,
        treasury: treasuryPda,
        funder: owner.publicKey,
        owner: owner.publicKey,
        systemProgram: SystemProgram.programId,
      })
      .signers([owner])
      .rpc();

    const treasuryBalance = await provider.connection.getBalance(treasuryPda);
    expect(treasuryBalance).to.be.at.least(depositAmount.toNumber());

    const vaultAccount = await program.account.vaultState.fetch(vaultPda);
    expect(vaultAccount.totalDeposited.toString()).to.equal(depositAmount.toString());
  });

  it("Prevents unauthorized caller from creating a stream", async () => {
    const streamDurationSecs = 10;
    const now = Math.floor(Date.now() / 1000);
    const endTime = new anchor.BN(now + streamDurationSecs);

    try {
      await program.methods
        .createStream(streamRate, endTime)
        .accounts({
          vault: vaultPda,
          stream: streamPda,
          recipient: recipient.publicKey,
          signer: unauthorizedAgent.publicKey,
          systemProgram: SystemProgram.programId,
        })
        .signers([unauthorizedAgent])
        .rpc();
      expect.fail("Unauthorized agent should have been blocked");
    } catch (err: any) {
      expect(err.toString()).to.contain("UnauthorizedAgentOrOwner");
    }
  });

  it("Allows authorized AI Agent to create an active stream", async () => {
    const streamDurationSecs = 4; // short duration for testing
    const now = Math.floor(Date.now() / 1000);
    const endTime = new anchor.BN(now + streamDurationSecs);

    await program.methods
      .createStream(streamRate, endTime)
      .accounts({
        vault: vaultPda,
        stream: streamPda,
        recipient: recipient.publicKey,
        signer: agent.publicKey,
        systemProgram: SystemProgram.programId,
      })
      .signers([agent])
      .rpc();

    const streamAccount = await program.account.streamState.fetch(streamPda);
    expect(streamAccount.recipient.toBase58()).to.equal(recipient.publicKey.toBase58());
    expect(streamAccount.ratePerSecond.toString()).to.equal(streamRate.toString());
  });

  it("Allows permissionless Keeper to crank stream and disburse lamports", async () => {
    // Wait 2 seconds to accumulate streaming balance
    await new Promise((resolve) => setTimeout(resolve, 2500));

    const recipientBalanceBefore = await provider.connection.getBalance(recipient.publicKey);

    await program.methods
      .crankStream()
      .accounts({
        vault: vaultPda,
        stream: streamPda,
        treasury: treasuryPda,
        recipient: recipient.publicKey,
        cranker: keeper.publicKey,
      })
      .signers([keeper])
      .rpc();

    const recipientBalanceAfter = await provider.connection.getBalance(recipient.publicKey);
    expect(recipientBalanceAfter).to.be.greaterThan(recipientBalanceBefore);
  });

  it("Closes stream and reclaims rent lamports after completion", async () => {
    // Wait until stream reaches end_time
    await new Promise((resolve) => setTimeout(resolve, 3000));

    // Final crank to flush accrued lamports
    await program.methods
      .crankStream()
      .accounts({
        vault: vaultPda,
        stream: streamPda,
        treasury: treasuryPda,
        recipient: recipient.publicKey,
        cranker: keeper.publicKey,
      })
      .signers([keeper])
      .rpc();

    const ownerBalanceBefore = await provider.connection.getBalance(owner.publicKey);

    await program.methods
      .closeStream()
      .accounts({
        vault: vaultPda,
        stream: streamPda,
        owner: owner.publicKey,
      })
      .signers([owner])
      .rpc();

    const ownerBalanceAfter = await provider.connection.getBalance(owner.publicKey);
    expect(ownerBalanceAfter).to.be.greaterThan(ownerBalanceBefore);

    // Verify account is closed
    const streamAccountInfo = await provider.connection.getAccountInfo(streamPda);
    expect(streamAccountInfo).to.be.null;
  });
});