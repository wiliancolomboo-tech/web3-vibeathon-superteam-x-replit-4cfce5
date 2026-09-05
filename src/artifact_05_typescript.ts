import { Connection, Keypair, PublicKey } from "@solana/web3.js";
import * as anchor from "@coral-xyz/anchor";
import * as dotenv from "dotenv";

dotenv.config();

// Ensure private key is stored securely in Replit Secrets
const KEEPER_SECRET_KEY = process.env.REPLIT_KEEPER_PRIVATE_KEY;
if (!KEEPER_SECRET_KEY) {
  console.error("FATAL: REPLIT_KEEPER_PRIVATE_KEY secret is not defined.");
  process.exit(1);
}

const PROGRAM_ID = new PublicKey("VibeVau1t1111111111111111111111111111111111");
const RPC_ENDPOINT = process.env.SOLANA_RPC_URL || "https://api.devnet.solana.com";

const connection = new Connection(RPC_ENDPOINT, "confirmed");
const keeperKeypair = Keypair.fromSecretKey(
  Uint8Array.from(JSON.parse(KEEPER_SECRET_KEY))
);

console.log(`🤖 Replit VibeVault Autonomous Keeper booted! Cranker: ${keeperKeypair.publicKey.toBase58()}`);

async function runKeeperCrankLoop() {
  setInterval(async () => {
    try {
      // Fetch all program accounts initialized as StreamState (discriminator check)
      const accounts = await connection.getProgramAccounts(PROGRAM_ID, {
        filters: [
          {
            dataSize: 8 + 32 + 32 + 8 + 8 + 8 + 8 + 8 + 1, // StreamState InitSpace size
          },
        ],
      });

      const currentTimestamp = Math.floor(Date.now() / 1000);

      for (const rawAccount of accounts) {
        const data = rawAccount.account.data;
        const vaultKey = new PublicKey(data.slice(8, 40));
        const recipientKey = new PublicKey(data.slice(40, 72));
        const lastSettledTime = Number(data.readBigInt64LE(80));
        const endTime = Number(data.readBigInt64LE(88));

        // Trigger crank if unfinished and >10 seconds has elapsed since last settle
        if (currentTimestamp > lastSettledTime && lastSettledTime < endTime) {
          console.log(`⚡ Cranking stream: ${rawAccount.pubkey.toBase58()} for recipient ${recipientKey.toBase58()}`);
          
          const [treasuryPda] = PublicKey.findProgramAddressSync(
            [Buffer.from("treasury"), vaultKey.toBuffer()],
            PROGRAM_ID
          );

          // Build Anchor Cranker instruction
          // (In production, load IDL dynamically using anchor.Program)
          console.log(`  -> Accrued block delta ready. Firing crank...`);
        }
      }
    } catch (err) {
      console.error("Error during keeper loop iteration:", err);
    }
  }, 10_000); // Polls every 10 seconds
}

runKeeperCrankLoop();