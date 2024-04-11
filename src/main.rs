#![warn(
    clippy::string_slice,
    clippy::str_to_string,
    clippy::inefficient_to_string,
    clippy::manual_string_new,
    clippy::map_unwrap_or,
    clippy::needless_pass_by_value,
    clippy::unused_self,
    clippy::explicit_iter_loop
)]

use std::error::Error;

use anyhow::Result;
use serde::Deserialize;
use tracing as log;
use twilight_gateway::{
    error::{ReceiveMessageError, ReceiveMessageErrorType},
    Event, Intents, Shard, ShardId,
};
use twilight_http::{request::AuditLogReason, Client};
use twilight_model::{
    application::interaction::{application_command::CommandData, Interaction, InteractionData},
    channel::message::{
        component::{ActionRow, Button, ButtonStyle},
        MessageType,
    },
    http::interaction::{InteractionResponse, InteractionResponseData, InteractionResponseType},
};

macro_rules! row {
    ($($component:expr),*) => {
        [ActionRow {
            components: vec![$($component.into(),)*]
        }.into()]
    };
}

macro_rules! link {
    ($label:expr, $url:expr) => {
        Button {
            style: ButtonStyle::Link,
            url: Some($url),
            custom_id: None,
            disabled: false,
            label: Some($label.to_owned()),
            emoji: None,
        }
    };
}

#[derive(Deserialize)]
struct Config {
    token: String,
}

#[tokio::main(worker_threads = 1)]
async fn main() -> Result<()> {
    // Parse the config and setup logger
    tracing_subscriber::fmt::init();

    let token = {
        let config = tokio::fs::read_to_string("config.json").await?;
        serde_json::from_str::<Config>(config.as_str())?.token
    };

    // Setup http and gateway connection (as minimal as possible)
    let http = Client::new(token.clone());
    let mut shard = Shard::new(ShardId::ONE, token.clone(), Intents::GUILD_MESSAGES);

    let mut user_id = None;
    log::info!("Connection established. Listening for events...");
    loop {
        let result = shard.next_event().await;
        match result {
            Ok(Event::Ready(ready)) => {
                user_id = Some(ready.user.id);
            }
            Ok(Event::InteractionCreate(ref interaction)) => {
                if let Some(InteractionData::ApplicationCommand(ref data)) = interaction.data {
                    if let Err(e) = handle_command(interaction, data, &http).await {
                        log::error!("Command failed: {e}");
                    }
                }
            }
            Ok(Event::MessageCreate(message)) => {
                // Delete the default "x pinned message" message in the channel, since we send our own!
                if user_id == Some(message.author.id)
                    && message.kind == MessageType::ChannelMessagePinned
                {
                    if let Err(e) = http.delete_message(message.channel_id, message.id).await {
                        log::error!("Failed to delete pin message: {e}");
                    }
                }
            }
            Err(error) => {
                log::error!(?error, "Error in event loop");
                if error.is_fatal() {
                    break;
                }
            }
            _ => {}
        }
    }
    Ok(())
}

const DEFER: InteractionResponse = InteractionResponse {
    kind: InteractionResponseType::DeferredChannelMessageWithSource,
    data: None,
};

#[inline]
fn guild_only() -> InteractionResponse {
    InteractionResponse {
        kind: InteractionResponseType::ChannelMessageWithSource,
        data: Some(InteractionResponseData {
            content: Some(
                "You can't pin messages in a direct message channel. Try in a server instead!"
                    .to_owned(),
            ),
            ..Default::default()
        }),
    }
}

async fn handle_command(event: &Interaction, data: &CommandData, http: &Client) -> Result<()> {
    let channel_id = event
        .channel
        .as_ref()
        .map(|c| c.id)
        .expect("Message command must have a channel id");
    let client = http.interaction(event.application_id);

    // Only allow pinning in guilds
    let Some(guild_id) = event.guild_id else {
        client
            .create_response(event.id, &event.token, &guild_only())
            .await?;
        return Ok(());
    };

    // Check that we are responding to the right command
    let pin = match data.name.as_str() {
        "Pin Message" => true,
        "Unpin Message" => false,
        _ => return Ok(()),
    };

    // Pull the message data used for pinning
    let message = data
        .resolved
        .as_ref()
        .and_then(|it| it.messages.values().next())
        .expect("Message command is missing resolved message!");

    // Acknowledge the interaction before doing anything else
    client
        .create_response(event.id, &event.token, &DEFER)
        .await?;

    let channel_name: &str = event
        .channel
        .as_ref()
        .and_then(|channel| channel.name.as_deref())
        .unwrap_or("");
    let username = &event.author().unwrap().name;

    // Pin or unpin the message
    let result = if pin {
        http.create_pin(channel_id, message.id)
            .reason(&format!("{username} pinned a message in {channel_name}"))
            .unwrap()
            .await
    } else {
        http.delete_pin(channel_id, message.id)
            .reason(&format!("{username} unpinned a message in {channel_name}"))
            .unwrap()
            .await
    };

    let button = row!(link!(
        "Message",
        format!(
            "https://discord.com/channels/{}/{}/{}",
            guild_id, channel_id, message.id
        )
    ));

    let request = client.create_followup(&event.token);

    if let Err(e) = result {
        // Could happen if we are missing permissions
        log::error!("Failed to process pin due to error: {}", e);
        request
            .content("Encountered some error, sorry about that... Try again?")?
            .await?;
    } else {
        // Send final response
        let content = format!(
            "\u{1F4CC} **{}** {}pinned message in this channel.",
            username,
            if pin { "" } else { "un" }
        );

        log::info!("[{}] {}", channel_id, content);
        request.components(&button)?.content(&content)?.await?;
    }

    Ok(())
}
