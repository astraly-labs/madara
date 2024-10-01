//! ExEx of Pragma Dispatcher
//! Adds a new TX at the end of each block, dispatching a message through
//! Hyperlane.

use futures::StreamExt;
use mp_exex::{ExExContext, ExExEvent};
use serde_json::Value;

async fn get_random_user() -> Result<(), Box<dyn std::error::Error>> {
    let url = "https://randomuser.me/api/";

    let response = reqwest::get(url).await?;
    let body = response.text().await?;
    let data: Value = serde_json::from_str(&body)?;

    if let Some(results) = data["results"].as_array() {
        if let Some(user) = results.first() {
            if let (Some(name), Some(email), Some(location)) =
                (user["name"].as_object(), user["email"].as_str(), user["location"]["city"].as_str())
            {
                log::info!("👤 Random User Information:");
                log::info!("Name: {} {}", name["first"].as_str().unwrap_or(""), name["last"].as_str().unwrap_or(""));
                log::info!("Email: {}", email);
                log::info!("City: {}", location);
            }
        }
    }

    Ok(())
}

pub async fn exex_pragma_dispatch(mut ctx: ExExContext) -> anyhow::Result<()> {
    while let Some(notification) = ctx.notifications.next().await {
        let block_number = notification.closed_block();
        log::info!("👋 Hello from the ExEx (#{})", block_number);

        // Fetch and print random user information
        if let Err(e) = get_random_user().await {
            log::error!("😱 Failed to fetch random user: {}", e);
        }

        ctx.events.send(ExExEvent::FinishedHeight(block_number))?;
    }
    Ok(())
}
