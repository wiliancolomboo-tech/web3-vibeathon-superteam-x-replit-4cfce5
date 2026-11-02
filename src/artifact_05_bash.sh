# 1. Clone repository & navigate to workspace
git clone https://github.com/superteam-replit/vibemesh.git
cd vibemesh

# 2. Install TypeScript QA dependencies
yarn install

# 3. Synchronize program keys
anchor keys sync

# 4. Compile Anchor Rust program with release optimizations
anchor build

# 5. Run complete QA verification suite on local validator
anchor test