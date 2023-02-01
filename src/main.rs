mod client;

use bazuka::core::{Amount, Header, Money, MpnAddress, MpnDeposit};
use bazuka::wallet::{TxBuilder, Wallet};
use bazuka::zk::MpnTransaction;
use chrono::prelude::*;
use client::SyncClient;
use colored::Colorize;
use hyper::header::HeaderValue;
use hyper::server::conn::AddrStream;
use hyper::service::{make_service_fn, service_fn};
use hyper::{Body, Client, Method, Request, Response, Server, StatusCode};
use rust_randomx::{Context, Hasher};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::io::{Read, Write};
use std::net::SocketAddr;
use std::path::Path;
use std::sync::Arc;
use structopt::StructOpt;
use thiserror::Error;
use tokio::sync::RwLock;

const LISTEN: &'static str = "0.0.0.0:8766";

#[derive(Error, Debug)]
pub enum PoolError {
    #[error("server error happened: {0}")]
    ServerError(#[from] hyper::Error),
    #[error("client error happened: {0}")]
    ClientError(#[from] hyper::http::Error),
    #[error("serde json error happened: {0}")]
    JsonError(#[from] serde_json::Error),
    #[error("bincode error happened: {0}")]
    BincodeError(#[from] bincode::Error),
    #[error("io error happened: {0}")]
    IoError(#[from] std::io::Error),
    #[error("from-hex error happened: {0}")]
    FromHexError(#[from] hex::FromHexError),
    #[error("mpn-address parse error happened: {0}")]
    MpnAddressError(#[from] bazuka::core::ParseMpnAddressError),
    #[error("node error happened: {0}")]
    BazukaNodeError(#[from] bazuka::client::NodeError),
}

#[derive(Serialize, Deserialize, Clone, PartialEq, Debug)]
pub struct Solution {
    pub nonce: String,
}

#[derive(Debug, StructOpt, Clone)]
#[structopt(name = "Uzi Pool", about = "Mine Ziesha with Uzi!")]
struct Opt {
    #[structopt(short = "n", long = "node")]
    node: SocketAddr,

    #[structopt(long, default_value = LISTEN)]
    listen: SocketAddr,

    #[structopt(long, default_value = "mainnet")]
    network: String,

    #[structopt(long, default_value = "")]
    miner_token: String,

    #[structopt(long, default_value = "10")]
    share_easiness: usize,

    #[structopt(long, default_value = "10")]
    share_capacity: usize,

    #[structopt(long, default_value = "5")]
    reward_delay: u64,

    #[structopt(long, default_value = "0.01")]
    owner_reward_ratio: f32,

    #[structopt(long)]
    pool_mpn_address: MpnAddress,
}

#[derive(Serialize, Deserialize, Clone, PartialEq, Debug)]
struct Share {
    miner: Miner,
    nonce: String,
}

#[derive(Serialize, Deserialize, Clone, PartialEq, Debug)]
struct Job {
    puzzle: Puzzle,
    shares: Vec<Share>,
    nonces: HashSet<String>,
}

#[derive(Serialize, Deserialize, Clone, PartialEq, Debug)]
struct Puzzle {
    key: String,
    blob: String,
    offset: usize,
    size: usize,
    target: u32,
    reward: Amount,
}

#[derive(Serialize, Deserialize, Clone, PartialEq, Debug)]
struct PuzzleWrapper {
    puzzle: Option<Puzzle>,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
struct History {
    solved: HashMap<Header, HashMap<MpnAddress, Amount>>,
    sent: HashMap<Header, (MpnDeposit, Vec<MpnTransaction>)>,
}

fn save_history(h: &History) -> Result<(), PoolError> {
    let history_path = home::home_dir()
        .unwrap()
        .join(Path::new(".uzi-pool-history"));
    File::create(history_path)?.write_all(&bincode::serialize(h)?)?;
    Ok(())
}

fn get_history() -> Result<History, PoolError> {
    let history_path = home::home_dir()
        .unwrap()
        .join(Path::new(".uzi-pool-history"));
    Ok(if let Ok(mut f) = File::open(history_path.clone()) {
        let mut bytes = Vec::new();
        f.read_to_end(&mut bytes)?;
        drop(f);
        bincode::deserialize(&bytes)?
    } else {
        History {
            solved: HashMap::new(),
            sent: HashMap::new(),
        }
    })
}

#[derive(Serialize, Deserialize, Clone, PartialEq, Debug)]
struct AddMinerRequest {
    mpn_addr: String,
}

#[derive(Serialize, Deserialize, Clone, PartialEq, Debug)]
struct AddMinerResponse {
    miner_token: String,
}

fn job_solved(
    total_reward: Amount,
    owner_reward_ratio: f32,
    shares: &[Share],
) -> HashMap<MpnAddress, Amount> {
    let total_reward_64 = Into::<u64>::into(total_reward);
    let owner_reward_64 = (total_reward_64 as f64 * owner_reward_ratio as f64) as u64;
    let per_share_reward: Amount =
        ((total_reward_64 - owner_reward_64) / (shares.len() as u64)).into();
    let mut rewards: HashMap<MpnAddress, Amount> = HashMap::new();
    for share in shares {
        *rewards.entry(share.miner.mpn_addr.clone()).or_default() += per_share_reward;
    }
    rewards
}

fn fetch_miner_token(req: &Request<Body>) -> Option<String> {
    if let Some(v) = req.headers().get("X-ZIESHA-MINER-TOKEN") {
        v.to_str().map(|s| s.into()).ok()
    } else {
        None
    }
}

fn generate_miner_token() -> String {
    use rand::distributions::Alphanumeric;
    use rand::{thread_rng, Rng};
    thread_rng()
        .sample_iter(&Alphanumeric)
        .take(30)
        .map(char::from)
        .collect()
}

async fn process_request(
    context: Arc<RwLock<MinerContext>>,
    request: Request<Body>,
    client: Option<SocketAddr>,
    opt: &Opt,
) -> Result<Response<Body>, PoolError> {
    let mut ctx = context.write().await;
    let miners = ctx.eligible_miners.clone();
    let miner = if let Some(Some(miner)) = fetch_miner_token(&request).map(|tkn| miners.get(&tkn)) {
        Some(miner.clone())
    } else {
        None
    };
    let is_local = client.map(|c| c.ip().is_loopback()).unwrap_or(true);
    let url = request.uri().path().to_string();
    match &url[..] {
        "/get-miners" => {
            if !is_local {
                let mut resp = Response::new(Body::empty());
                *resp.status_mut() = StatusCode::FORBIDDEN;
                return Ok(resp);
            } else {
                let miners: HashMap<String, String> = miners
                    .into_iter()
                    .map(|(k, v)| (k, v.mpn_addr.to_string()))
                    .collect();
                let mut resp = Response::new(serde_json::to_string(&miners).unwrap().into());
                resp.headers_mut().insert(
                    "Content-Type",
                    HeaderValue::from_str("application/json").unwrap(),
                );
                return Ok(resp);
            }
        }
        "/add-miner" => {
            if !is_local {
                let mut resp = Response::new(Body::empty());
                *resp.status_mut() = StatusCode::FORBIDDEN;
                return Ok(resp);
            } else {
                let miners_path = home::home_dir()
                    .unwrap()
                    .join(Path::new(".uzi-pool-miners"));
                let body = request.into_body();
                let body_bytes = hyper::body::to_bytes(body).await?;

                let add_miner_req: AddMinerRequest = serde_json::from_slice(&body_bytes)?;
                let mut miners = miners.clone().into_values().collect::<Vec<Miner>>();
                let miner_token = generate_miner_token();
                miners.push(Miner {
                    mpn_addr: add_miner_req.mpn_addr.parse()?,
                    token: miner_token.clone(),
                });
                File::create(miners_path)?.write_all(&serde_json::to_vec(&miners)?)?;
                let mut resp = Response::new(
                    serde_json::to_string(&AddMinerResponse { miner_token })
                        .unwrap()
                        .into(),
                );
                resp.headers_mut().insert(
                    "Content-Type",
                    HeaderValue::from_str("application/json").unwrap(),
                );

                return Ok(resp);
            }
        }
        "/miner/puzzle" => {
            if miner.is_none() {
                log::warn!("Miner not authorized!");
                let mut resp = Response::new(Body::empty());
                *resp.status_mut() = StatusCode::FORBIDDEN;
                return Ok(resp);
            };

            let easy_puzzle = ctx.current_job.as_ref().map(|j| {
                let mut new_pzl = j.puzzle.clone();
                new_pzl.target = rust_randomx::Difficulty::new(new_pzl.target)
                    .scale(1f32 / (opt.share_easiness as f32))
                    .to_u32();
                new_pzl
            });
            let mut resp = Response::new(
                serde_json::to_string(&PuzzleWrapper {
                    puzzle: easy_puzzle.clone(),
                })
                .unwrap()
                .into(),
            );
            resp.headers_mut().insert(
                "Content-Type",
                HeaderValue::from_str("application/json").unwrap(),
            );

            return Ok(resp);
        }
        "/miner/solution" => {
            let miner = if let Some(miner) = miner {
                miner
            } else {
                log::warn!("Miner not authorized!");
                let mut resp = Response::new(Body::empty());
                *resp.status_mut() = StatusCode::FORBIDDEN;
                return Ok(resp);
            };
            let body = request.into_body();
            let body_bytes = hyper::body::to_bytes(body).await?;
            let sol: Solution = serde_json::from_slice(&body_bytes)?;

            let mut block_solved: Option<(Header, HashMap<MpnAddress, Amount>)> = None;
            let hasher = Hasher::new(ctx.hasher.clone());
            if let Some(current_job) = ctx.current_job.as_mut() {
                let easy_puzzle = {
                    let mut new_pzl = current_job.puzzle.clone();
                    new_pzl.target = rust_randomx::Difficulty::new(new_pzl.target)
                        .scale(1f32 / (opt.share_easiness as f32))
                        .to_u32();
                    new_pzl
                };

                let block_diff = rust_randomx::Difficulty::new(current_job.puzzle.target);
                let share_diff = rust_randomx::Difficulty::new(easy_puzzle.target);
                let mut blob = hex::decode(easy_puzzle.blob.clone())?;
                let header: Header = bincode::deserialize(&blob)?;
                let (b, e) = (easy_puzzle.offset, easy_puzzle.offset + easy_puzzle.size);
                blob[b..e].copy_from_slice(&hex::decode(&sol.nonce)?);
                let out = hasher.hash(&blob);

                if out.meets_difficulty(share_diff) {
                    if !current_job.nonces.contains(&sol.nonce) {
                        current_job.shares.push(Share {
                            miner: miner.clone(),
                            nonce: sol.nonce.clone(),
                        });
                        current_job.nonces.insert(sol.nonce.clone());
                        while current_job.shares.len() > opt.share_capacity {
                            current_job.shares.remove(0);
                        }
                        if out.meets_difficulty(block_diff) {
                            block_solved = Some((
                                header,
                                job_solved(
                                    current_job.puzzle.reward,
                                    opt.owner_reward_ratio,
                                    &current_job.shares,
                                ),
                            ));

                            println!(
                                "{} -> {} {}",
                                Local::now(),
                                "Solution found by:".bright_green(),
                                miner.token
                            );
                            let req = Request::builder()
                                .method(Method::POST)
                                .uri(format!("http://{}/miner/solution", opt.node))
                                .header("X-ZIESHA-MINER-TOKEN", &opt.miner_token)
                                .body(json!({ "nonce": sol.nonce }).to_string().into())?;
                            let client = Client::new();
                            client.request(req).await?;
                        } else {
                            println!(
                                "{} -> {} {}",
                                Local::now(),
                                "Share found by:".bright_green(),
                                miner.token
                            );
                        }
                    } else {
                        log::warn!(
                            "{} {}",
                            "Duplicated share submitted by:".bright_green(),
                            miner.token
                        );
                    }
                }
            }
            if let Some((header, entries)) = block_solved {
                let mut h = get_history()?;
                h.solved.insert(header, entries);
                save_history(&h)?;
                ctx.current_job = None;
            }
            return Ok(Response::new(Body::empty()));
        }
        _ => {}
    }

    Ok(Response::new(Body::empty()))
}

async fn new_puzzle(
    context: Arc<RwLock<MinerContext>>,
    req: PuzzleWrapper,
) -> Result<(), PoolError> {
    let mut ctx = context.write().await;
    if ctx.current_job.as_ref().map(|j| j.puzzle.clone()) == req.puzzle {
        return Ok(());
    }
    if let Some(req) = req.puzzle.clone() {
        ctx.current_job = Some(Job {
            puzzle: req.clone(),
            shares: vec![],
            nonces: HashSet::new(),
        });
        let req_key = hex::decode(&req.key)?;

        if ctx.hasher.key() != req_key {
            println!("{}", "Initializing hasher...".bright_yellow());
            ctx.hasher = Arc::new(Context::new(&req_key, false));
        }

        let target = rust_randomx::Difficulty::new(req.target);
        println!(
            "{} Approximately {} hashes need to be calculated...",
            "Got new puzzle!".bright_yellow(),
            target.power()
        );
    } else {
        ctx.current_job = None;
        println!("No puzzles to mine...");
    }

    Ok(())
}

#[derive(Serialize, Deserialize, Clone, PartialEq, Debug)]
struct Miner {
    token: String,
    mpn_addr: MpnAddress,
}

struct MinerContext {
    client: SyncClient,
    hasher: Arc<Context>,
    current_job: Option<Job>,
    eligible_miners: HashMap<String, Miner>,
}

fn create_tx(
    wallet: &mut Wallet,
    memo: String,
    entries: HashMap<MpnAddress, Amount>,
    remote_nonce: u32,
    remote_mpn_nonce: u64,
    pool_mpn_address: MpnAddress,
) -> Result<(bazuka::core::MpnDeposit, Vec<MpnTransaction>), PoolError> {
    let mpn_id = bazuka::config::blockchain::get_blockchain_config().mpn_contract_id;
    let tx_builder = TxBuilder::new(&wallet.seed());
    let new_nonce = wallet.new_r_nonce().unwrap_or(remote_nonce + 1);
    let new_mpn_nonce = wallet
        .new_z_nonce(pool_mpn_address.account_index)
        .unwrap_or(remote_mpn_nonce);
    let sum_all = entries
        .iter()
        .map(|(_, m)| Into::<u64>::into(*m))
        .sum::<u64>();
    let tx = tx_builder.deposit_mpn(
        memo,
        mpn_id,
        pool_mpn_address.clone(),
        0,
        new_nonce,
        Money::ziesha(sum_all),
        Money::ziesha(0),
    );
    wallet.add_deposit(tx.clone());
    let mut ztxs = Vec::new();
    for (i, (addr, mon)) in entries.iter().enumerate() {
        let tx = tx_builder.create_mpn_transaction(
            pool_mpn_address.account_index,
            0,
            addr.clone(),
            0,
            Money::ziesha((*mon).into()),
            0,
            Money::ziesha(0),
            new_mpn_nonce + i as u64,
        );
        wallet.add_zsend(tx.clone());
        ztxs.push(tx);
    }
    Ok((tx, ztxs))
}

fn get_miners() -> Result<HashMap<String, Miner>, PoolError> {
    let miners_path = home::home_dir()
        .unwrap()
        .join(Path::new(".uzi-pool-miners"));
    Ok(if let Ok(mut f) = File::open(miners_path.clone()) {
        let mut bytes = Vec::new();
        f.read_to_end(&mut bytes)?;
        let miners_bincode: Option<Vec<Miner>> = bincode::deserialize(&bytes).ok();
        if let Some(miners) = miners_bincode {
            File::create(miners_path)?.write_all(&serde_json::to_vec(&miners)?)?;
            miners
        } else {
            serde_json::from_slice(&bytes)?
        }
    } else {
        Vec::new()
    }
    .into_iter()
    .map(|m| (m.token.clone(), m))
    .collect())
}

#[tokio::main]
async fn main() -> Result<(), ()> {
    println!(
        "{} v{} - RandomX Mining Pool for Ziesha Cryptocurrency",
        "Uzi-Pool!".bright_green(),
        env!("CARGO_PKG_VERSION")
    );

    env_logger::init();
    let opt = Opt::from_args();
    println!("{} {}", "Listening to:".bright_yellow(), opt.listen);
    if opt.owner_reward_ratio > 1.0 || opt.owner_reward_ratio < 0.0 {
        println!("Owner reward ratio should be between 0.0 and 1.0!")
    }

    let context = Arc::new(RwLock::new(MinerContext {
        client: SyncClient::new(
            bazuka::client::PeerAddress(opt.node),
            &opt.network,
            opt.miner_token.clone(),
        ),
        current_job: None,
        hasher: Arc::new(Context::new(b"", false)),
        eligible_miners: get_miners().unwrap(),
    }));

    // Construct our SocketAddr to listen on...
    let addr = SocketAddr::from(opt.listen);

    // And a MakeService to handle each connection...
    let ctx_server = Arc::clone(&context);
    let opt_server = opt.clone();
    let make_service = make_service_fn(move |conn: &AddrStream| {
        let client = conn.remote_addr();
        let opt = opt_server.clone();
        let ctx = Arc::clone(&ctx_server);
        async move {
            let opt = opt.clone();
            let ctx = Arc::clone(&ctx);
            Ok::<_, PoolError>(service_fn(move |req: Request<Body>| {
                let opt = opt.clone();
                let ctx = Arc::clone(&ctx);
                async move {
                    let resp = process_request(ctx, req, Some(client), &opt).await?;
                    Ok::<_, PoolError>(resp)
                }
            }))
        }
    });

    // Then bind and serve...
    let server = async {
        Server::bind(&addr)
            .http1_only(true)
            .http1_keepalive(false)
            .serve(make_service)
            .await?;
        Ok::<(), PoolError>(())
    };

    let ctx_puzzle_getter = Arc::clone(&context);
    let opt_puzzle_getter = opt.clone();
    let puzzle_getter = async move {
        loop {
            let ctx = Arc::clone(&ctx_puzzle_getter);
            let opt = opt_puzzle_getter.clone();
            if let Err(e) = async move {
                let req = Request::builder()
                    .uri(format!("http://{}/miner/puzzle", opt.node))
                    .header("X-ZIESHA-MINER-TOKEN", &opt.miner_token)
                    .body(Body::empty())?;
                let client = Client::new();
                let resp = hyper::body::to_bytes(client.request(req).await?.into_body()).await?;
                let pzl_json: PuzzleWrapper = serde_json::from_slice(&resp)?;
                new_puzzle(ctx.clone(), pzl_json).await?;
                Ok::<_, PoolError>(())
            }
            .await
            {
                log::error!("Error: {}", e);
            }
            tokio::time::sleep(std::time::Duration::from_secs(5)).await;
        }
        Ok::<(), PoolError>(())
    };

    let ctx_reward_sender = Arc::clone(&context);
    let opt_reward_sender = opt.clone();
    let reward_sender = {
        let ctx = Arc::clone(&ctx_reward_sender);
        let opt = opt_reward_sender.clone();
        async move {
            loop {
                let ctx = Arc::clone(&ctx);
                let opt = opt.clone();
                if let Err(e) = async move {
                    let mut ctx = ctx.write().await;
                    ctx.eligible_miners = get_miners()?;
                    let mut hist = get_history()?;
                    let curr_height = ctx.client.get_height().await?;
                    let wallet_path = home::home_dir().unwrap().join(Path::new(".bazuka-wallet"));
                    let mut wallet = Wallet::open(wallet_path.clone()).unwrap().unwrap();
                    let curr_nonce = ctx
                        .client
                        .get_account(TxBuilder::new(&wallet.seed()).get_address())
                        .await?
                        .account
                        .nonce;
                    let curr_mpn_nonce = ctx
                        .client
                        .get_mpn_account(opt.pool_mpn_address.account_index)
                        .await?
                        .account
                        .nonce;
                    for (h, entries) in hist.solved.clone().into_iter() {
                        if let Some(mut actual_header) = ctx.client.get_header(h.number).await? {
                            actual_header.proof_of_work.nonce = 0;
                            if curr_height - h.number >= opt.reward_delay {
                                hist.solved.remove(&h);
                                if actual_header == h {
                                    let (tx, ztxs) = create_tx(
                                        &mut wallet,
                                        format!("Pool-Reward, block #{}", h.number).into(),
                                        entries,
                                        curr_nonce,
                                        curr_mpn_nonce,
                                        opt.pool_mpn_address.clone(),
                                    )?;
                                    wallet.save(wallet_path.clone()).unwrap();
                                    println!("Tx with nonce {} created...", tx.payment.nonce);
                                    hist.sent.insert(h, (tx, ztxs));
                                }
                                save_history(&hist)?;
                            }
                        }
                    }
                    println!("Current nonce: {}", curr_nonce);
                    let mut sent_sorted = hist.sent.clone().into_iter().collect::<Vec<_>>();
                    sent_sorted.sort_unstable_by_key(|s| s.1 .0.payment.nonce);
                    for (h, (tx, ztxs)) in sent_sorted {
                        let last_ztx = ztxs.iter().last().unwrap();
                        if tx.payment.nonce > curr_nonce || last_ztx.nonce >= curr_mpn_nonce {
                            println!(
                                "Sending rewards for block #{} (Nonce: {})...",
                                h.number, tx.payment.nonce
                            );
                            ctx.client.transact_deposit(tx.clone()).await?;
                            for ztx in ztxs {
                                ctx.client.transact_zero(ztx.clone()).await?;
                            }
                        } else {
                            println!("Tx with nonce {} removed...", tx.payment.nonce);
                            save_history(&hist)?;
                        }
                    }

                    Ok::<_, PoolError>(())
                }
                .await
                {
                    log::error!("Error: {}", e);
                }
                tokio::time::sleep(std::time::Duration::from_secs(60)).await;
            }

            Ok::<(), PoolError>(())
        }
    };

    // And run forever...
    if let Err(e) = tokio::try_join!(server, puzzle_getter, reward_sender) {
        eprintln!("error: {}", e);
    }

    Ok(())
}
