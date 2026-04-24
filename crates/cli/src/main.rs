use std::net::SocketAddr;
use std::sync::Arc;

use limits::Tracker;
use memory::testing::HashEmbedder;
use memory::{MemoryConfig, Store, UserId};
use prompter::{Config, Prompter};
use server::{AppState, Server};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let config = Config::from_path("coulisse.yaml")?;
    let default_user_id = config.default_user_id.as_deref().map(UserId::from_string);
    let prompter = Prompter::new(config).await?;
    let memory = Store::new(HashEmbedder::default(), MemoryConfig::default());
    let tracker = Tracker::new();
    let state = Arc::new(AppState {
        default_user_id,
        memory,
        prompter,
        tracker,
    });
    let addr = SocketAddr::from(([0, 0, 0, 0], 8421));
    println!("coulisse listening on http://{addr}");
    println!(
        "  memory: HashEmbedder (MVP placeholder — swap for a real embedder before production)"
    );
    for agent in state.prompter.agents() {
        println!(
            "  agent: {} (provider={}, model={})",
            agent.name,
            agent.provider.as_str(),
            agent.model,
        );
    }
    Server::new(addr, state).run().await?;
    Ok(())
}
