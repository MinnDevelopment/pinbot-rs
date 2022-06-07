use futures::StreamExt;
use serde::Deserialize;
use twilight_gateway::{shard::ShardBuilder, Event, Intents};
use twilight_http::Client;
use twilight_model::{
    application::{
        component::{button::ButtonStyle, ActionRow, Button, Component},
        interaction::{ApplicationCommand, Interaction},
    },
    channel::message::MessageType,
    http::interaction::{InteractionResponse, InteractionResponseData, InteractionResponseType},
};

#[derive(Deserialize)]
struct Config {
    token: String,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    // Parse the config and setup logger
    simple_logger::init_with_level(log::Level::Info).unwrap();
    let config = tokio::fs::read_to_string("config.json").await?;
    let token = serde_json::from_str::<Config>(config.as_str())?.token;

    // Setup http and gateway connection (as minimal as possible)
    let http = Client::new(token.clone());
    let (shard, mut events) = ShardBuilder::new(token.clone(), Intents::GUILD_MESSAGES)
        .shard(0, 1)?
        .gateway_url("wss://gateway.discord.gg".to_string())
        .build()
        .await?;

    shard.start().await?;

    let mut user = None;
    log::info!("Connection established. Listening for events...");
    while let Some(event) = events.next().await {
        match event {
            Event::Ready(ready) => {
                user = Some(ready.user.id);
            }
            Event::InteractionCreate(interaction) => {
                if let Interaction::ApplicationCommand(command) = interaction.0 {
                    if let Err(e) = handle_command(command, &http).await {
                        log::error!("Command failed: {}", e);
                    }
                }
            }
            Event::MessageCreate(message) => {
                // Delete the default "x pinned message" message in the channel, since we send our own!
                if user.map_or(false, |id| id == message.author.id)
                    && message.kind == MessageType::ChannelMessagePinned
                {
                    if let Err(e) = http
                        .delete_message(message.channel_id, message.id)
                        .exec()
                        .await
                    {
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

fn guild_only() -> InteractionResponse {
    InteractionResponse {
        kind: InteractionResponseType::ChannelMessageWithSource,
        data: Some(InteractionResponseData {
            allowed_mentions: None,
            attachments: None,
            choices: None,
            components: None,
            content: Some(
                "You can't pin messages in a direct message channel. Try in a server instead!"
                    .to_string(),
            ),
            custom_id: None,
            embeds: None,
            flags: None,
            title: None,
            tts: None,
        }),
    }
}

fn row(component: Component) -> [Component; 1] {
    [Component::ActionRow(ActionRow {
        components: vec![component],
    })]
}

async fn handle_command(
    event: Box<ApplicationCommand>,
    http: &Client,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let client = http.interaction(event.application_id);
    // Only allow pinning in guilds
    let guild_id = if let Some(id) = event.guild_id {
        id
    } else {
        client
            .create_response(event.id, &event.token, &guild_only())
            .exec()
            .await?;
        return Ok(());
    };

    // Check that we are responding to the right command
    let name = event.data.name.as_str();
    let pin = match name {
        "Pin Message" => true,
        "Unpin Message" => false,
        _ => return Ok(()),
    };

    // Pull the message data used for pinning
    let resolved = event
        .data
        .resolved
        .as_ref()
        .expect("Message command is missing resolved message!");
    let message = resolved.messages.values().into_iter().next().unwrap();

    // Acknowledge the interaction before doing anything else
    let client = http.interaction(event.application_id);
    client
        .create_response(event.id, &event.token, &DEFER)
        .exec()
        .await?;

    // Pin or unpin the message
    let result = if pin {
        http.create_pin(message.channel_id, message.id).exec().await
    } else {
        http.delete_pin(message.channel_id, message.id).exec().await
    };

    let button = Component::Button(Button {
        custom_id: None,
        disabled: false,
        emoji: None,
        label: Some("Link".to_string()),
        style: ButtonStyle::Link,
        url: Some(format!(
            "https://discord.com/channels/{}/{}/{}",
            guild_id, message.channel_id, message.id
        )),
    });

    if let Err(e) = result {
        // Could happen if we are missing permissions
        log::error!("Failed to process pin due to error: {}", e);
        client
            .create_followup(&event.token)
            .content("Encountered some error, sorry about that... Try again?")?
            .exec()
            .await?;
    } else {
        // Send final response
        let content = format!(
            "\u{1F4CC} **{}** {}pinned message in this channel.",
            get_tag(&event),
            if pin { "" } else { "un" }
        );
        log::info!("[{}] {}", event.channel_id, content);
        client
            .create_followup(&event.token)
            .components(&row(button))?
            .content(content.as_str())?
            .exec()
            .await?;
    }

    Ok(())
}

/// Gets the user tag as {name}#{discriminator} for an application command event
fn get_tag(event: &ApplicationCommand) -> String {
    if let Some(ref user) = event.user {
        format!("{}#{}", user.name, user.discriminator)
    } else if let Some(user) = event.member.as_ref().and_then(|m| m.user.as_ref()) {
        format!("{}#{}", user.name, user.discriminator)
    } else {
        panic!("Could not resolve user for event!")
    }
}
