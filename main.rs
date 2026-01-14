use ethers::{
    prelude::*,
    providers::{Provider, Ws},
    utils::{parse_ether, format_ether},
    abi::{Token, encode},
    types::{TransactionRequest, Eip1559TransactionRequest},
};
use ethers_flashbots::{BundleRequest, FlashbotsMiddleware};
use petgraph::{graph::{NodeIndex, UnGraph}, visit::EdgeRef};
use std::{sync::Arc, collections::HashMap, str::FromStr, net::TcpListener, io::Write, thread};
use colored::*;
use dotenv::dotenv;
use std::env;
use anyhow::{Result, anyhow};
use url::Url;
use log::{info, warn, error};

// --- CONFIG ---
#[derive(Clone, Debug)]
struct ChainConfig {
    name: String,
    rpc_env: String,
    rpc_default: String,
    flashbots: String,
    is_l2: bool,
}

// --- ABIGEN ---
abigen!(
    IUniswapV2Pair,
    r#"[
        function getReserves() external view returns (uint112 reserve0, uint112 reserve1, uint32 blockTimestampLast)
        function token0() external view returns (address)
        function token1() external view returns (address)
    ]"#
);

abigen!(
    ApexOmega,
    r#"[ function execute(uint256 mode, address token, uint256 amount, bytes calldata strategy) external payable ]"#
);

#[derive(Clone, Copy, Debug)]
struct PoolEdge {
    pair_address: Address,
    token_0: Address,
    token_1: Address,
    reserve_0: U256,
    reserve_1: U256,
    fee_numerator: u32,
}

#[tokio::main]
async fn main() -> Result<()> {
    dotenv().ok();
    env_logger::builder().filter_level(log::LevelFilter::Info).init();

    println!("{}", "╔════════════════════════════════════════════════════════╗".gold());
    println!("{}", "║    ⚡ APEX TITAN v87.0 (RUST SINGULARITY EDITION)    ║".gold());
    println!("{}", "║    MODES: SATURATION BROADCAST | INFINITE RECURSION    ║".gold());
    println!("{}", "╚════════════════════════════════════════════════════════╝".gold());

    // 1. HARDENED VALIDATION
    validate_env()?;

    // 2. CLOUD BOOT GUARD (Port 8080 Health Check)
    // Matches JS functionality to keep cloud instances alive
    thread::spawn(|| {
        let listener = TcpListener::bind("0.0.0.0:8080").unwrap();
        info!("Cloud Health Monitor Active on Port 8080");
        for stream in listener.incoming() {
            if let Ok(mut s) = stream {
                let _ = s.write_all(b"HTTP/1.1 200 OK\r\n\r\n{\"status\":\"TITAN_ONLINE\"}");
            }
        }
    });

    // 3. MULTI-CHAIN CONFIGURATION (Quad-Core)
    let chains = vec![
        ChainConfig { name: "ETHEREUM".into(), rpc_env: "ETH_RPC".into(), rpc_default: "wss://eth.llamarpc.com".into(), flashbots: "https://relay.flashbots.net".into(), is_l2: false },
        ChainConfig { name: "BASE".into(), rpc_env: "BASE_RPC".into(), rpc_default: "wss://base.llamarpc.com".into(), flashbots: "".into(), is_l2: true },
        ChainConfig { name: "ARBITRUM".into(), rpc_env: "ARB_RPC".into(), rpc_default: "wss://arbitrum.llamarpc.com".into(), flashbots: "".into(), is_l2: true },
        ChainConfig { name: "POLYGON".into(), rpc_env: "POLY_RPC".into(), rpc_default: "wss://polygon-bor.publicnode.com".into(), flashbots: "".into(), is_l2: true },
    ];

    let pk = env::var("PRIVATE_KEY")?;
    let exec = env::var("EXECUTOR_ADDRESS")?;

    // 4. SPAWN ASYNC MONITORS
    let mut handles = vec![];
    for config in chains {
        let k = pk.clone();
        let e = exec.clone();
        handles.push(tokio::spawn(async move {
            if let Err(err) = monitor_chain(config.clone(), k, e).await {
                error!("[{}] Chain Died: {:?}", config.name, err);
            }
        }));
    }

    // Keep main process alive
    for h in handles { let _ = h.await; }
    Ok(())
}

// --- CORE LOGIC PER CHAIN ---
async fn monitor_chain(config: ChainConfig, pk: String, exec_addr: String) -> Result<()> {
    let rpc_url = env::var(&config.rpc_env).unwrap_or(config.rpc_default);
    info!("[{}] Connecting...", config.name);

    let provider = Provider::<Ws>::connect(&rpc_url).await?;
    let provider = Arc::new(provider);
    let wallet: LocalWallet = pk.parse()?;
    let chain_id = provider.get_chainid().await?.as_u64();
    let client = Arc::new(SignerMiddleware::new(provider.clone(), wallet.clone().with_chain_id(chain_id)));

    // --- FEATURE: STARTUP PING ---
    // Sends 0 ETH to self to verify write access (matches JS Startup Ping)
    info!("[{}] 🧪 Initiating Startup Ping...", config.name);
    let ping_tx = TransactionRequest::new().to(wallet.address()).value(0);
    match client.send_transaction(ping_tx, None).await {
        Ok(tx) => info!("[{}] ✅ Ping Success: {:?}", config.name, tx.tx_hash()),
        Err(e) => warn!("[{}] ⚠️ Ping Failed (Check Gas/Balance): {:?}", config.name, e),
    }

    // Setup Flashbots
    let fb_client = if !config.flashbots.is_empty() {
        Some(FlashbotsMiddleware::new(client.clone(), Url::parse(&config.flashbots)?, wallet.clone()))
    } else { None };

    let executor = ApexOmega::new(exec_addr.parse::<Address>()?, client.clone());

    // --- GRAPH BUILDER ---
    let mut graph = UnGraph::<Address, PoolEdge>::new_undirected();
    let mut node_map: HashMap<Address, NodeIndex> = HashMap::new();
    let mut pair_map: HashMap<Address, petgraph::graph::EdgeIndex> = HashMap::new();

    // Production: Load 20k+ pools here
    let pools = vec!["0xB4e16d0168e52d35CaCD2c6185b44281Ec28C9Dc"]; 

    info!("[{}] Loading Graph...", config.name);
    for pool in pools {
        if let Ok(addr) = Address::from_str(pool) {
            let pair = IUniswapV2Pair::new(addr, provider.clone());
            if let Ok((r0, r1, _)) = pair.get_reserves().call().await {
                let t0 = pair.token0().call().await?;
                let t1 = pair.token1().call().await?;
                let n0 = *node_map.entry(t0).or_insert_with(|| graph.add_node(t0));
                let n1 = *node_map.entry(t1).or_insert_with(|| graph.add_node(t1));
                let idx = graph.add_edge(n0, n1, PoolEdge {
                    pair_address: addr, token_0: t0, token_1: t1, reserve_0: r0.into(), reserve_1: r1.into(), fee_numerator: 997,
                });
                pair_map.insert(addr, idx);
            }
        }
    }

    info!("[{}] Armed. Threshold: 0.001 ETH", config.name);

    let filter = Filter::new().event("Sync(uint112,uint112)");
    let mut stream = provider.subscribe_logs(&filter).await?;

    while let Some(log) = stream.next().await {
        // Balance Safety Check (Matches JS Threshold)
        let bal = provider.get_balance(wallet.address(), None).await.unwrap_or_default();
        if bal < parse_ether("0.001")? { continue; } 

        if let Some(idx) = pair_map.get(&log.address) {
            if let Some(edge) = graph.edge_weight_mut(*idx) {
                let r0 = U256::from_big_endian(&log.data[0..32]);
                let r1 = U256::from_big_endian(&log.data[32..64]);
                edge.reserve_0 = r0;
                edge.reserve_1 = r1;
            }

            // RECURSIVE LOGIC
            let weth = if chain_id == 137 { "0x0d500B1d8E8eF31E21C99d1Db9A6444d3ADf1270" } else { "0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2" };
            if let Some(start) = node_map.get(&Address::from_str(weth)?) {
                let amt_in = parse_ether("10")?;
                
                // Infinite Hops (Depth 4)
                if let Some((profit, route)) = find_arb_recursive(&graph, *start, *start, amt_in, 4, vec![]) {
                    if profit > parse_ether("0.05")? {
                        info!("[{}] 💎 OPPORTUNITY: {} ETH", config.name, format_ether(profit));
                        
                        let bribe = profit * 90 / 100;
                        let strategy = build_strategy(route, amt_in, bribe, executor.address(), &graph)?;

                        let tx = executor.execute(U256::zero(), Address::from_str(weth)?, amt_in, strategy).tx;

                        // --- FEATURE: SATURATION BROADCAST ---
                        if let Some(fb) = fb_client.as_ref() {
                            let block = provider.get_block_number().await?;
                            let bundle = BundleRequest::new()
                                .push_transaction(tx)
                                .set_block(block + 1)
                                .set_simulation_block(block)
                                .set_simulation_timestamp(0);
                            fb.send_bundle(&bundle).await.ok();
                        } else {
                            // DUAL CHANNEL: Standard Provider + Raw HTTP
                            // Need http url for raw post (convert wss to https)
                            let http_url = rpc_url.replace("wss://", "https://").replace("ws://", "http://");
                            saturation_strike(&client, &http_url, tx).await;
                        }
                    }
                }
            }
        }
    }
    Ok(())
}

// --- FEATURE: SATURATION STRIKE (Dual-Channel) ---
// Broadcasts to RPC via HTTP Raw Post AND Standard Provider simultaneously
async fn saturation_strike<M: Middleware + 'static>(client: &Arc<M>, rpc_url: &str, tx: Eip1559TransactionRequest) {
    // 1. Sign
    if let Ok(signed_tx) = client.signer().sign_transaction(&tx.clone().into()).await {
        let raw_tx = signed_tx.rlp();
        
        // 2. HTTP Raw Post (Fire and Forget)
        let client_http = reqwest::Client::new();
        let rpc = rpc_url.to_string();
        let raw_tx_hex = format!("0x{}", hex::encode(&raw_tx));
        
        tokio::spawn(async move {
            let body = serde_json::json!({
                "jsonrpc": "2.0",
                "method": "eth_sendRawTransaction",
                "params": [raw_tx_hex],
                "id": 1
            });
            let _ = client_http.post(rpc).json(&body).send().await;
        });

        // 3. Provider Broadcast
        let _ = client.send_transaction(tx, None).await;
        info!("🚀 Saturation Strike Sent");
    }
}

// --- VALIDATION ---
fn validate_env() -> Result<()> {
    let key = env::var("PRIVATE_KEY").map_err(|_| anyhow!("Missing PRIVATE_KEY"))?;
    if key.len() != 64 && !key.starts_with("0x") { return Err(anyhow!("Invalid Private Key Length")); }
    let exec = env::var("EXECUTOR_ADDRESS").map_err(|_| anyhow!("Missing EXECUTOR_ADDRESS"))?;
    if exec.len() != 42 { return Err(anyhow!("Invalid Contract Address Length")); }
    Ok(())
}

// --- RECURSIVE DFS ---
fn find_arb_recursive(graph: &UnGraph<Address, PoolEdge>, curr: NodeIndex, start: NodeIndex, amt: U256, depth: u8, mut path: Vec<(Address, Address)>) -> Option<(U256, Vec<(Address, Address)>)> {
    if curr == start && path.len() > 1 {
        let initial = parse_ether("10").unwrap();
        return if amt > initial { Some((amt - initial, path)) } else { None };
    }
    if depth == 0 { return None; }

    for edge in graph.edges(curr) {
        let next = edge.target();
        if path.iter().any(|(a, _)| *a == *graph.node_weight(next).unwrap()) && next != start { continue; }
        
        let out = get_amount_out(amt, edge.weight(), curr, graph);
        if out.is_zero() { continue; }

        let mut next_path = path.clone();
        next_path.push((*graph.node_weight(curr).unwrap(), *graph.node_weight(next).unwrap()));
        
        if let Some(res) = find_arb_recursive(graph, next, start, out, depth - 1, next_path) {
            return Some(res);
        }
    }
    None
}

// --- MATH UTILS ---
fn get_amount_out(amt_in: U256, edge: &PoolEdge, curr: NodeIndex, graph: &UnGraph<Address, PoolEdge>) -> U256 {
    let addr = graph.node_weight(curr).unwrap();
    let (r_in, r_out) = if *addr == edge.token_0 { (edge.reserve_0, edge.reserve_1) } else { (edge.reserve_1, edge.reserve_0) };
    if r_in.is_zero() || r_out.is_zero() { return U256::zero(); }
    let amt_fee = amt_in * edge.fee_numerator;
    (amt_fee * r_out) / ((r_in * 1000) + amt_fee)
}

// --- ZERO-COPY ENCODER (Token Chaining) ---
fn build_strategy(route: Vec<(Address, Address)>, init_amt: U256, bribe: U256, contract: Address, graph: &UnGraph<Address, PoolEdge>) -> Result<Bytes> {
    let mut targets = Vec::new();
    let mut payloads = Vec::new();
    let mut curr_in = init_amt;
    let t_sig = [0xa9, 0x05, 0x9c, 0xbb]; 
    let s_sig = [0x02, 0x2c, 0x0d, 0x9f]; 

    for (i, (tin, tout)) in route.iter().enumerate() {
        let nin = graph.node_indices().find(|i| *graph.node_weight(*i).unwrap() == tin).unwrap();
        let nout = graph.node_indices().find(|i| *graph.node_weight(*i).unwrap() == tout).unwrap();
        let edge = &graph[graph.find_edge(nin, nout).unwrap()];

        if i == 0 {
            targets.push(tin);
            let mut d = t_sig.to_vec();
            d.extend(ethers::abi::encode(&[Token::Address(edge.pair_address), Token::Uint(init_amt)]));
            payloads.push(Bytes::from(d));
        }

        let out = get_amount_out(curr_in, edge, nin, graph);
        let (a0, a1) = if tin == edge.token_0 { (U256::zero(), out) } else { (out, U256::zero()) };
        let to = if i == route.len() - 1 { contract } else {
            let (n_next, n_next_out) = (nout, graph.node_indices().find(|i| *graph.node_weight(*i).unwrap() == route[i+1].1).unwrap());
            graph[graph.find_edge(n_next, n_next_out).unwrap()].pair_address
        };

        targets.push(edge.pair_address);
        let mut d = s_sig.to_vec();
        d.extend(ethers::abi::encode(&[Token::Uint(a0), Token::Uint(a1), Token::Address(to), Token::Bytes(vec![])]));
        payloads.push(Bytes::from(d));
        curr_in = out;
    }

    Ok(Bytes::from(encode(&[
        Token::Array(targets.into_iter().map(Token::Address).collect()),
        Token::Array(payloads.into_iter().map(Token::Bytes).collect()),
        Token::Uint(bribe),
    ])))
}
