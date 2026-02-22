mod parser;
mod watcher;
mod docker;
mod state;
mod ui;
mod app;

use anyhow::Result;
use app::App;

#[tokio::main]
async fn main() -> Result<()> {
    let lead_path = std::env::args().nth(1)
        .or_else(|| std::env::current_dir().ok().map(|p| p.to_string_lossy().into_owned()));
    let mut app = App::new(lead_path).await?;
    app.run().await?;
    Ok(())
}
