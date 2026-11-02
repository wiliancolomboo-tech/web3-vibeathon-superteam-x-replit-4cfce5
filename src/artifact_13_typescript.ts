import { Connection, Keypair, PublicKey } from "@solana/web3.js";
import { Program, AnchorProvider, Wallet } from "@coral-xyz/anchor";

// Load ephemeral agent key from Replit Secret Environment
const secretKey = JSON.parse(process.env.REPLIT_AGENT_SOLANA_KEY!);
const agentKeypair = Keypair.fromSecretKey(Uint8Array.from(secretKey));

const connection = new Connection("https://api.devnet.solana.com", "confirmed");
const provider = new AnchorProvider(connection, new Wallet(agentKeypair), {});
// vibeAgentProgram.methods.claimJob()...