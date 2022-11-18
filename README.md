# 🏊 Uzi Pool 🏊

Uzi-Pool is a very simple RandomX Mining Pool Software for Ziesha Cryptocurrency. 
If you want to design a pool for Ziesha, you might use this repo as a template!

The current payout method is PPLNS (Pay-Per-Last-N-Shares) and its parameters can be
tuned through command-line arguments.

As the owner of a pool, you will need to run `uzi-pool` instead of `uzi-miner`. 
Your pool members will run `uzi-miner`to connect to your pool. **You will need to 
run a MPN Executor (`zoro`) in order to run a pool, but the pool members won't need it!**

In fact, someone can join a pool by only installing `uzi-miner` (He can skip installing 
`bazuka` and `zoro`.

## How to use?

Prepare `bazuka` and `zoro` and make sure they are connected to each other correctly.

Install `uzi-pool`:

```
git clone https://github.com/ziesha-network/uzi-pool
cd uzi-pool
cargo install --path .
```

Now run it:

```
uzi-pool --node 127.0.0.1:8765 --share-capacity 40 --share-easiness 20 --reward-ratio 0.01
```

`--share-capacity` specifies the number of shares that are rewarded (E.g The last 40 shares are rewarded, it's the parameter N in PPLNS payout method)
`--share-easiness` specifies how easier a share is compared to the actual puzzle (E.g the difficulty is 20 times lower)
`--reward-ratio` specifies the amount of the reward going to the pool owner's wallet.

Rewards are given with a delay of 5 blocks (Which is tunable with `--reward-delay` parameter

