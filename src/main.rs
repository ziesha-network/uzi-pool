use colored::Colorize;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::error::Error;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::thread;
use structopt::StructOpt;
use tiny_http::{Response, Server};

const WEBHOOK: &'static str = "127.0.0.1:8766";

#[derive(Serialize, Deserialize, Clone, PartialEq, Debug)]
pub struct Solution {
    pub nonce: String,
}

#[derive(Debug, StructOpt, Clone)]
#[structopt(name = "Uzi Pool", about = "Mine Zeeka with Uzi!")]
struct Opt {
    #[structopt(short = "n", long = "node")]
    node: SocketAddr,

    #[structopt(long, default_value = "")]
    miner_token: String,
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
    match request.url() {
        "/miner/puzzle" => {
            request.respond(Response::from_string(
                serde_json::to_string(&context.lock().unwrap().current_puzzle).unwrap(),
            ))?;
        }
        "/miner/solution" => {
            let sol: Solution = {
                let mut content = String::new();
                request.as_reader().read_to_string(&mut content)?;
                serde_json::from_str(&content)?
            };
            ureq::post(&format!("http://{}/miner/solution", opt.node))
                .set("X-ZEEKA-MINER-TOKEN", &opt.miner_token)
                .send_json(json!({ "nonce": sol.nonce }))?;
            request.respond(Response::from_string("OK"))?;
        }
        _ => {}
    }
    Ok(())
}

fn new_puzzle(
    context: Arc<Mutex<MinerContext>>,
    request: RequestWrapper,
) -> Result<(), Box<dyn Error>> {
    if let Some(req) = &request.puzzle {
        let power = rust_randomx::Difficulty::new(req.target).power();
        println!(
            "{} Approximately {} hashes need to be calculated...",
            "Got new puzzle!".bright_yellow(),
            power
        );
    }
    context.lock().unwrap().current_puzzle = request;

    Ok(())
}

struct MinerContext {
    hasher_context: Option<Arc<rust_randomx::Context>>,
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

    let server = Server::http(WEBHOOK).unwrap();

    let context = Arc::new(Mutex::new(MinerContext {
        current_puzzle: RequestWrapper { puzzle: None },
        hasher_context: None,
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
