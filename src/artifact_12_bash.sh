# Set cluster configuration to devnet
solana config set --url https://api.devnet.solana.com

# Fund deployment wallet
solana airdrop 2

# Deploy program
anchor deploy --provider.cluster devnet