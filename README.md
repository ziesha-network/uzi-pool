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

Remove the old history and pool wallet:

```
rm -rf ~/.uzi-pool-history
rm -rf ~/.uzi-pool-miners
```

Now run it:

```
uzi-pool --node 127.0.0.1:8765 --share-capacity 40 --share-easiness 20 --owner-reward-ratio 0.01
```

 * `--share-capacity` specifies the number of shares that are rewarded (E.g The last 40 shares are rewarded, it's the parameter N in PPLNS payout method)
 * `--share-easiness` specifies how easier a share is compared to the actual puzzle (E.g the difficulty is 20 times lower)
 * `--owner-reward-ratio` specifies the amount of the reward going to the pool owner's wallet. (E.g 1% of block reward goes to the wallet of owner)

Rewards are given with a delay of 5 blocks (Which is tunable with `--reward-delay` parameter

## How to add miners?

Uzi-Pool will listen to port 8766 for requests, you can add a miner by sending a POST request to `127.0.0.1:8766/add-miner`, containing the wallet address of the new member, e.g:

```
curl -X POST 127.0.0.1:8766/add-miner -d '{"mpn_addr":"MPN ADDRESS OF MEMBER"}'
```

The response will be a JSON message containing a Miner-token which should be used by the miner when running Uzi-Miner.

Now the member can connect to the pool:

```
uzi-miner --pool --node IP_OF_THE_POOL:8766 --miner-token "MINER TOKEN OF MEMBER" --thread NUM_THREADS
```

***The miner should pass `--pool` flag to uzi-miner if he is mining on a pool, or his miner will not run correctly!***


## Which pools are better?

Mining process starts when a puzzle is ready. The puzzle is ready after a block is drafted by `zoro`. So a pool might waste his time drafting a block if he doesn't have a competetive hardware compared to other pools and not win blocks. (Even if it has a lot of pool members). `zoro` is in charge of drafting blocks, and a pool-owner with more powerful GPU/GPUs has the chance to start the mining process sooner than opponents and win blocks with higher chance.
