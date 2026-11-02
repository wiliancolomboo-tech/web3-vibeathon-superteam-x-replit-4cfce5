import * as anchor from "@coral-xyz/anchor";
import { Program } from "@coral-xyz/anchor";
import { Vibestream } from "../target/types/vibestream";
import { Connection, Keypair } from "@solana/web3.js";

async function runAutonomousCrank() {
  const connection = new Connection("https://api.devnet.solana.com", "confirmed");
  const keeperKeypair = Keypair.fromSecretKey(
    Buffer.from(JSON.parse(process.env.KEEPER_PRIVATE_KEY!))
  );
  const provider = new anchor.AnchorProvider(
    connection,
    new anchor.Wallet(keeperKeypair),
    { commitment: "confirmed" }
  );
  const program = anchor.workspace.Vibestream as Program<Vibestream>;

  console.log("VibeStream Keeper Crank Active. Scanning task accounts...");

  setInterval(async () => {
    try {
      const tasks = await program.account.taskState.all();
      const now = Math.floor(Date.now() / 1000);

      for (const task of tasks) {
        const status = task.account.status;
        const isSubmitted = "submitted" in status;
        const isClaimedAndExpired = "claimed" in status && now > task.account.expiresAt.toNumber();

        if (isSubmitted || isClaimedAndExpired) {
          console.log(`Settling eligible task: ${task.publicKey.toBase58()}`);
          const [vaultPda] = anchor.web3.PublicKey.findProgramAddressSync(
            [Buffer.from("vault"), task.publicKey.toBuffer()],
            program.programId
          );

          await program.methods
            .settleTask()
            .accounts({
              crankKeeper: keeperKeypair.publicKey,
              task: task.publicKey,
              vault: vaultPda,
              requester: task.account.requester,
              worker: task.account.worker,
              protocolTreasury: keeperKeypair.publicKey, // Or designated treasury
              systemProgram: anchor.web3.SystemProgram.programId,
            })
            .signers([keeperKeypair])
            .rpc();

          console.log(`Task ${task.publicKey.toBase58()} settled successfully.`);
        }
      }
    } catch (err) {
      console.error("Crank iteration error:", err);
    }
  }, 10_000);
}

runAutonomousCrank();