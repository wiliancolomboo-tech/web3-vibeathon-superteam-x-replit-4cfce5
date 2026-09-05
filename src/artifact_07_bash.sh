# 1. Clone or extract project repository
cd vibe_vault

# 2. Install Node dependencies
yarn install

# 3. Generate a local test validator ledger keypair if needed
solana-keygen new --no-bip39-passphrase -o ~/.config/solana/id.json --force

# 4. Synchronize Program ID with declared key
anchor keys sync

# 5. Build the Anchor program binary and verify zero warnings
anchor build

# 6. Execute full End-to-End Test Matrix
anchor test