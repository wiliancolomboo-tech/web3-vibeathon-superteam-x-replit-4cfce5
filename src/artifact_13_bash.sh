# 1. Switch to Devnet
solana config set --url https://api.devnet.solana.com

# 2. Airdrop SOL to deployment keypair
solana airdrop 2

# 3. Deploy to Solana Devnet
anchor deploy --provider.cluster devnet

# 4. Run the Replit Agent Runner Daemon
python3 keeper/agent_runner.py