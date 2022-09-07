use anyhow::Result;
use futures::StreamExt;
use serde::Deserialize;
use tokio::sync::OnceCell;
use tracing as log;
use twilight_gateway::{shard::ShardBuilder, Event, Intents};
use twilight_http::Client;
use twilight_model::{
    application::{
        component::{button::ButtonStyle, ActionRow, Button, Component},
        interaction::{application_command::CommandData, Interaction, InteractionData},
    },
    channel::message::MessageType,
    http::interaction::{InteractionResponse, InteractionResponseData, InteractionResponseType},
};

macro_rules! row {
    ($button:expr) => {
        [Component::ActionRow(ActionRow {
            components: vec![$button.into()],
        })]
    };
}

macro_rules! send {
    ($request:expr) => {
        $request.exec().await
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

    let config = tokio::fs::read_to_string("config.json").await?;
    let token = serde_json::from_str::<Config>(config.as_str())?.token;

    // Setup http and gateway connection (as minimal as possible)
    let http = Client::new(token.clone());
    let (shard, mut events) = ShardBuilder::new(token.clone(), Intents::GUILD_MESSAGES)
        .shard(0, 1)?
        .gateway_url("wss://gateway.discord.gg".to_string())
        .build();

    shard.start().await?;

    let user = OnceCell::new();
    log::info!("Connection established. Listening for events...");
    while let Some(event) = events.next().await {
        match event {
            Event::Ready(ready) => {
                user.get_or_init(|| async { ready.user.id }).await;
            }
            Event::InteractionCreate(ref interaction) => {
                if let Some(InteractionData::ApplicationCommand(ref data)) = interaction.data {
                    if let Err(e) = handle_command(interaction, data, &http).await {
                        log::error!("Command failed: {}", e);
                    }
                }
            }
            Event::MessageCreate(message) => {
                // Delete the default "x pinned message" message in the channel, since we send our own!
                if user.get() == Some(&message.author.id)
                    && message.kind == MessageType::ChannelMessagePinned
                {
                    if let Err(e) = send! { http.delete_message(message.channel_id, message.id) } {
                        log::error!("Failed to delete pin message: {}", e);
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
                    .to_string(),
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
    let guild_id = if let Some(id) = event.guild_id {
        id
    } else {
        send! { client.create_response(event.id, &event.token, &guild_only()) }?;
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
    send! { client.create_response(event.id, &event.token, &DEFER) }?;

    // Pin or unpin the message
    let result = if pin {
        send! { http.create_pin(channel_id, message.id) }
    } else {
        send! { http.delete_pin(channel_id, message.id) }
    };

    let button = row! {
        Button {
            label: Some("Link".to_string()),
            style: ButtonStyle::Link,
            url: Some(format!(
                "https://discord.com/channels/{}/{}/{}",
                guild_id, channel_id, message.id
            )),
            custom_id: None,
            disabled: false,
            emoji: None,
        }
    };

    let request = client.create_followup(&event.token);

    if let Err(e) = result {
        // Could happen if we are missing permissions
        log::error!("Failed to process pin due to error: {}", e);
        send! { request.content("Encountered some error, sorry about that... Try again?")? }?;
    } else {
        // Send final response
        let content = format!(
            "\u{1F4CC} **{}** {}pinned message in this channel.",
            get_tag(event),
            if pin { "" } else { "un" }
        );

        log::info!("[{}] {}", channel_id, content);
        send! { request.components(&button)?.content(&content)? }?;
    }

    Ok(())
}

/// Gets the user tag as {name}#{discriminator} for an application command event
#[inline]
#[allow(clippy::or_fun_call)]
fn get_tag(event: &Interaction) -> String {
    event
        .user
        .as_ref()
        .or(event.member.as_ref().and_then(|m| m.user.as_ref()))
        .map(|user| format!("{}#{}", user.name, user.discriminator))
        .expect("Could not resolve user for event!")
}
