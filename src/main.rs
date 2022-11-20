mod client;

use bazuka::core::{Address, Header, Money, RegularSendEntry};
use bazuka::wallet::{TxBuilder, Wallet};
use client::SyncClient;
use colored::Colorize;
use rust_randomx::{Context, Hasher};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::HashMap;
use std::error::Error;
use std::fs::File;
use std::io::{Read, Write};
use std::net::SocketAddr;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::thread;
use structopt::StructOpt;
use tiny_http::{Response, Server};

const LISTEN: &'static str = "0.0.0.0:8766";

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
}

#[derive(Serialize, Deserialize, Clone, PartialEq, Debug)]
struct Puzzle {
    key: String,
    blob: String,
    offset: usize,
    size: usize,
    target: u32,
    reward: Money,
}

#[derive(Serialize, Deserialize, Clone, PartialEq, Debug)]
struct PuzzleWrapper {
    puzzle: Option<Puzzle>,
}

#[derive(Serialize, Deserialize, Clone, PartialEq, Debug)]
struct History {
    solved: HashMap<Header, Vec<RegularSendEntry>>,
}

#[derive(Serialize, Deserialize, Clone, PartialEq, Debug)]
struct AddMinerRequest {
    pub_key: String,
}

#[derive(Serialize, Deserialize, Clone, PartialEq, Debug)]
struct AddMinerResponse {
    miner_token: String,
}

fn job_solved(
    total_reward: Money,
    owner_reward_ratio: f32,
    shares: &[Share],
) -> Vec<RegularSendEntry> {
    let total_reward_64 = Into::<u64>::into(total_reward);
    let owner_reward_64 = (total_reward_64 as f64 * owner_reward_ratio as f64) as u64;
    let per_share_reward: Money =
        ((total_reward_64 - owner_reward_64) / (shares.len() as u64)).into();
    let mut rewards: HashMap<Address, Money> = HashMap::new();
    for share in shares {
        *rewards.entry(share.miner.pub_key.clone()).or_default() += per_share_reward;
    }
    rewards
        .into_iter()
        .map(|(k, v)| RegularSendEntry { dst: k, amount: v })
        .collect()
}

fn fetch_miner_token(req: &tiny_http::Request) -> Option<String> {
    for h in req.headers() {
        if h.field.equiv("X-ZIESHA-MINER-TOKEN") {
            return Some(h.value.clone().into());
        }
    }
    None
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

fn process_request(
    context: Arc<Mutex<MinerContext>>,
    mut request: tiny_http::Request,
    opt: &Opt,
) -> Result<(), Box<dyn Error>> {
    let mut ctx = context.lock().unwrap();
    let miners = get_miners()?;
    let miner = if let Some(Some(miner)) = fetch_miner_token(&request).map(|tkn| miners.get(&tkn)) {
        Some(miner.clone())
    } else {
        None
    };

    match request.url() {
        "/get-miners" => {
            if !request.remote_addr().ip().is_loopback() {
                request.respond(Response::from_string("ERR"))?;
            } else {
                let miners: HashMap<String, String> = miners
                    .into_iter()
                    .map(|(k, v)| (k, v.pub_key.to_string()))
                    .collect();
                let mut resp = Response::from_string(serde_json::to_string(&miners).unwrap());
                resp.add_header(
                    tiny_http::Header::from_bytes(&b"Content-Type"[..], &b"application/json"[..])
                        .unwrap(),
                );
                request.respond(resp)?;
            }
        }
        "/add-miner" => {
            if !request.remote_addr().ip().is_loopback() {
                request.respond(Response::from_string("ERR"))?;
            } else {
                let miners_path = home::home_dir()
                    .unwrap()
                    .join(Path::new(".uzi-pool-miners"));
                let add_miner_req: AddMinerRequest = {
                    let mut content = String::new();
                    request.as_reader().read_to_string(&mut content)?;
                    serde_json::from_str(&content)?
                };
                let mut miners = miners.clone().into_values().collect::<Vec<Miner>>();
                let miner_token = generate_miner_token();
                miners.push(Miner {
                    pub_key: add_miner_req.pub_key.parse()?,
                    token: miner_token.clone(),
                });
                File::create(miners_path)?.write_all(&bincode::serialize(&miners)?)?;
                let mut resp = Response::from_string(
                    serde_json::to_string(&AddMinerResponse { miner_token }).unwrap(),
                );
                resp.add_header(
                    tiny_http::Header::from_bytes(&b"Content-Type"[..], &b"application/json"[..])
                        .unwrap(),
                );
                request.respond(resp)?;
            }
        }
        "/miner/puzzle" => {
            if miner.is_none() {
                log::warn!("Miner not authorized!");
                request.respond(Response::empty(401))?;
                return Ok(());
            };

            let easy_puzzle = ctx.current_job.as_ref().map(|j| {
                let mut new_pzl = j.puzzle.clone();
                new_pzl.target = rust_randomx::Difficulty::new(new_pzl.target)
                    .scale(1f32 / (opt.share_easiness as f32))
                    .to_u32();
                new_pzl
            });
            request.respond(Response::from_string(
                serde_json::to_string(&PuzzleWrapper {
                    puzzle: easy_puzzle.clone(),
                })
                .unwrap(),
            ))?;
        }
        "/miner/solution" => {
            let miner = if let Some(miner) = miner {
                miner
            } else {
                log::warn!("Miner not authorized!");
                request.respond(Response::empty(401))?;
                return Ok(());
            };

            let sol: Solution = {
                let mut content = String::new();
                request.as_reader().read_to_string(&mut content)?;
                serde_json::from_str(&content)?
            };

            let mut block_solved: Option<(Header, Vec<RegularSendEntry>)> = None;
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
                    current_job.shares.push(Share {
                        miner: miner.clone(),
                        nonce: sol.nonce.clone(),
                    });
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

                        println!("{} {}", "Solution found by:".bright_green(), miner.token);
                        ureq::post(&format!("http://{}/miner/solution", opt.node))
                            .set("X-ZIESHA-MINER-TOKEN", &opt.miner_token)
                            .send_json(json!({ "nonce": sol.nonce }))?;
                    } else {
                        println!("{} {}", "Share found by:".bright_green(), miner.token);
                    }
                    request.respond(Response::empty(200))?;
                }
            }
            if let Some((header, entries)) = block_solved {
                ctx.history.insert(header, entries);
                ctx.current_job = None;
            }
        }
        _ => {}
    }
    Ok(())
}

fn new_puzzle(context: Arc<Mutex<MinerContext>>, req: PuzzleWrapper) -> Result<(), Box<dyn Error>> {
    let mut ctx = context.lock().unwrap();
    if ctx.current_job.as_ref().map(|j| j.puzzle.clone()) == req.puzzle {
        return Ok(());
    }
    if let Some(req) = req.puzzle.clone() {
        ctx.current_job = Some(Job {
            puzzle: req.clone(),
            shares: vec![],
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
    pub_key: Address,
}

struct MinerContext {
    history: HashMap<Header, Vec<RegularSendEntry>>,
    client: SyncClient,
    hasher: Arc<Context>,
    current_job: Option<Job>,
}

fn send_tx(client: &SyncClient, entries: Vec<RegularSendEntry>) -> Result<(), Box<dyn Error>> {
    let wallet_path = home::home_dir().unwrap().join(Path::new(".bazuka-wallet"));
    let mut wallet = Wallet::open(wallet_path.clone())
        .unwrap()
        .unwrap_or_else(|| Wallet::create(&mut rand_mnemonic::thread_rng(), None));
    let tx_builder = TxBuilder::new(&wallet.seed());
    let curr_nonce = client.get_account(tx_builder.get_address())?.account.nonce;
    let new_nonce = wallet.new_r_nonce().unwrap_or(curr_nonce + 1);
    let tx = tx_builder.create_multi_transaction(entries, 0.into(), new_nonce);
    client.transact(tx.clone())?;
    wallet.add_rsend(tx);
    wallet.save(wallet_path).unwrap();
    Ok(())
}

fn get_miners() -> Result<HashMap<String, Miner>, Box<dyn Error>> {
    let miners_path = home::home_dir()
        .unwrap()
        .join(Path::new(".uzi-pool-miners"));
    Ok(if let Ok(mut f) = File::open(miners_path) {
        let mut bytes = Vec::new();
        f.read_to_end(&mut bytes)?;
        let miners: Vec<Miner> = bincode::deserialize(&bytes)?;
        miners
    } else {
        Vec::new()
    }
    .into_iter()
    .map(|m| (m.token.clone(), m))
    .collect())
}

fn main() {
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

    let server = Server::http(opt.listen).unwrap();
    let context = Arc::new(Mutex::new(MinerContext {
        history: HashMap::new(),
        client: SyncClient::new(
            bazuka::client::PeerAddress(opt.node),
            &opt.network,
            opt.miner_token.clone(),
        ),
        current_job: None,
        hasher: Arc::new(Context::new(b"", false)),
    }));

    let puzzle_getter = {
        let ctx = Arc::clone(&context);
        let opt = opt.clone();
        thread::spawn(move || loop {
            if let Err(e) = || -> Result<(), Box<dyn Error>> {
                let pzl = ureq::get(&format!("http://{}/miner/puzzle", opt.node))
                    .set("X-ZIESHA-MINER-TOKEN", &opt.miner_token)
                    .call()?
                    .into_string()?;

                let pzl_json: PuzzleWrapper = serde_json::from_str(&pzl)?;
                new_puzzle(ctx.clone(), pzl_json)?;
                Ok(())
            }() {
                log::error!("Error: {}", e);
            }
            std::thread::sleep(std::time::Duration::from_secs(5));
        })
    };

    let reward_sender = {
        let ctx = Arc::clone(&context);
        let opt = opt.clone();
        thread::spawn(move || loop {
            if let Err(e) = || -> Result<(), Box<dyn Error>> {
                let mut ctx = ctx.lock().unwrap();
                let max_ind = ctx
                    .history
                    .iter()
                    .map(|(h, _)| h.number)
                    .max()
                    .unwrap_or_default();
                for (h, entries) in ctx.history.clone() {
                    if max_ind - h.number >= opt.reward_delay {
                        send_tx(&ctx.client, entries)?;
                        ctx.history.remove(&h);
                    }
                }
                Ok(())
            }() {
                log::error!("Error: {}", e);
            }
            std::thread::sleep(std::time::Duration::from_secs(5));
        })
    };

    for request in server.incoming_requests() {
        if let Err(e) = process_request(context.clone(), request, &opt) {
            log::error!("Error: {}", e);
        }
    }

    puzzle_getter.join().unwrap();
    reward_sender.join().unwrap();
}
