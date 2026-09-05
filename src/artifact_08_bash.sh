# 1. Configure Solana CLI to target Devnet
solana config set --url https://api.devnet.solana.com

# 2. Airdrop SOL to your deployer wallet
solana airdrop 2

# 3. Deploy the binary to Devnet
anchor deploy --provider.cluster devnet

# 4. In Replit, set the Environment Secrets:
# SOLANA_RPC_URL="https://api.devnet.solana.com"
# REPLIT_KEEPER_PRIVATE_KEY="[...keypair bytes...]"

# 5. Run the Autonomous Keeper Bot in Replit Console
npx ts-node bot/agent_keeper.ts