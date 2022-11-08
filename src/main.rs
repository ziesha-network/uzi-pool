use colored::Colorize;
use rust_randomx::{Context, Hasher};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::error::Error;
use std::net::SocketAddr;
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
#[structopt(name = "Uzi Pool", about = "Mine Zeeka with Uzi!")]
struct Opt {
    #[structopt(short = "n", long = "node")]
    node: SocketAddr,

    #[structopt(long, default_value = LISTEN)]
    listen: SocketAddr,

    #[structopt(long, default_value = "")]
    miner_token: String,

    #[structopt(long, default_value = "10")]
    share_easiness: usize,

    #[structopt(long, default_value = "10")]
    share_capacity: usize,
}

#[derive(Serialize, Deserialize, Clone, PartialEq, Debug)]
struct Share {
    pub_key: String,
    nonce: String,
}

#[derive(Serialize, Deserialize, Clone, PartialEq, Debug)]
struct Job {
    puzzle: Request,
    shares: Vec<Share>,
}

#[derive(Serialize, Deserialize, Clone, PartialEq, Debug)]
struct Request {
    key: String,
    blob: String,
    offset: usize,
    size: usize,
    target: u32,
}

#[derive(Serialize, Deserialize, Clone, PartialEq, Debug)]
struct RequestWrapper {
    puzzle: Option<Request>,
}

fn process_request(
    context: Arc<Mutex<MinerContext>>,
    mut request: tiny_http::Request,
    opt: &Opt,
) -> Result<(), Box<dyn Error>> {
    let mut ctx = context.lock().unwrap();

    let mut easy_puzzle = ctx.current_puzzle.clone();
    easy_puzzle.puzzle.as_mut().map(|mut p| {
        p.target = rust_randomx::Difficulty::new(p.target)
            .scale(1f32 / (opt.share_easiness as f32))
            .to_u32()
    });

    match request.url() {
        "/miner/puzzle" => {
            request.respond(Response::from_string(
                serde_json::to_string(&easy_puzzle).unwrap(),
            ))?;
        }
        "/miner/solution" => {
            let sol: Solution = {
                let mut content = String::new();
                request.as_reader().read_to_string(&mut content)?;
                serde_json::from_str(&content)?
            };

            if let Some((block_puzzle, puzzle)) =
                ctx.current_puzzle.puzzle.as_ref().zip(easy_puzzle.puzzle)
            {
                let block_diff = rust_randomx::Difficulty::new(block_puzzle.target);
                let share_diff = rust_randomx::Difficulty::new(puzzle.target);
                let mut blob = hex::decode(puzzle.blob)?;
                let (b, e) = (puzzle.offset, puzzle.offset + puzzle.size);
                blob[b..e].copy_from_slice(&hex::decode(&sol.nonce)?);
                let out = ctx.hasher.hash(&blob);

                if out.meets_difficulty(share_diff) {
                    if out.meets_difficulty(block_diff) {
                        ctx.current_puzzle.puzzle = None;
                        println!("{} {}", "Solution found by:".bright_green(), "");
                        ureq::post(&format!("http://{}/miner/solution", opt.node))
                            .set("X-ZEEKA-MINER-TOKEN", &opt.miner_token)
                            .send_json(json!({ "nonce": sol.nonce }))?;
                    } else {
                        println!("{} {}", "Share found by:".bright_green(), "");
                    }
                    request.respond(Response::from_string("OK"))?;
                }
            }
        }
        _ => {}
    }
    Ok(())
}

fn new_puzzle(
    context: Arc<Mutex<MinerContext>>,
    request: RequestWrapper,
) -> Result<(), Box<dyn Error>> {
    let mut ctx = context.lock().unwrap();
    ctx.current_puzzle = request.clone();
    if let Some(req) = &request.puzzle {
        let req_key = hex::decode(&req.key)?;

        if ctx.hasher.context().key() != req_key {
            println!("{}", "Initializing hasher...".bright_yellow());
            ctx.hasher = Hasher::new(Arc::new(rust_randomx::Context::new(&req_key, false)));
        }

        let target = rust_randomx::Difficulty::new(req.target);
        println!(
            "{} Approximately {} hashes need to be calculated...",
            "Got new puzzle!".bright_yellow(),
            target.power()
        );
    }

    Ok(())
}

struct MinerContext {
    hasher: rust_randomx::Hasher,
    current_puzzle: RequestWrapper,
}

fn main() {
    println!(
        "{} v{} - RandomX Mining Pool for Zeeka Cryptocurrency",
        "Uzi-Pool!".bright_green(),
        env!("CARGO_PKG_VERSION")
    );

    env_logger::init();
    let opt = Opt::from_args();
    println!("{} {}", "Listening to:".bright_yellow(), opt.listen);

    let server = Server::http(opt.listen).unwrap();

    let context = Arc::new(Mutex::new(MinerContext {
        current_puzzle: RequestWrapper { puzzle: None },
        hasher: Hasher::new(Arc::new(Context::new(b"", false))),
    }));

    let puzzle_getter = {
        let ctx = Arc::clone(&context);
        let opt = opt.clone();
        thread::spawn(move || loop {
            if let Err(e) = || -> Result<(), Box<dyn Error>> {
                let pzl = ureq::get(&format!("http://{}/miner/puzzle", opt.node))
                    .set("X-ZEEKA-MINER-TOKEN", &opt.miner_token)
                    .call()?
                    .into_string()?;

                let pzl_json: RequestWrapper = serde_json::from_str(&pzl)?;
                if ctx.lock()?.current_puzzle != pzl_json.clone() {
                    new_puzzle(ctx.clone(), pzl_json)?;
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
}
