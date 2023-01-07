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

use anyhow::Result;
use futures::StreamExt;
use serde::Deserialize;
use tracing as log;
use twilight_gateway::{shard::ShardBuilder, Event, Intents};
use twilight_http::Client;
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
    let (shard, mut events) = ShardBuilder::new(token.clone(), Intents::GUILD_MESSAGES)
        .shard(0, 1)?
        .gateway_url("wss://gateway.discord.gg".to_owned())
        .build();

    shard.start().await?;

    let mut user_id = None;
    log::info!("Connection established. Listening for events...");
    while let Some(event) = events.next().await {
        match event {
            Event::Ready(ready) => {
                user_id = Some(ready.user.id);
            }
            Event::InteractionCreate(ref interaction) => {
                if let Some(InteractionData::ApplicationCommand(ref data)) = interaction.data {
                    if let Err(e) = handle_command(interaction, data, &http).await {
                        log::error!("Command failed: {e}");
                    }
                }
            }
            Event::MessageCreate(message) => {
                // Delete the default "x pinned message" message in the channel, since we send our own!
                if user_id == Some(message.author.id)
                    && message.kind == MessageType::ChannelMessagePinned
                {
                    if let Err(e) = http.delete_message(message.channel_id, message.id).await {
                        log::error!("Failed to delete pin message: {e}");
                    }
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
        .channel_id
        .expect("Message command must have a channel id");
    let client = http.interaction(event.application_id);

    // Only allow pinning in guilds
    let Some(guild_id) = event.guild_id else {
        client.create_response(event.id, &event.token, &guild_only()).await?;
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

    // Pin or unpin the message
    let result = if pin {
        http.create_pin(channel_id, message.id).await
    } else {
        http.delete_pin(channel_id, message.id).await
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
            get_tag(event),
            if pin { "" } else { "un" }
        );

        log::info!("[{}] {}", channel_id, content);
        request.components(&button)?.content(&content)?.await?;
    }

    Ok(())
}

/// Gets the user tag as {name}#{discriminator} for an application command event
#[inline]
#[allow(clippy::or_fun_call)]
fn get_tag(event: &Interaction) -> String {
    event
        .author()
        .map(|user| format!("{}#{}", user.name, user.discriminator()))
        .expect("Could not resolve user for event!")
}
