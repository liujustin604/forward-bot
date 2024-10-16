use anyhow::Context as _;
use nonzero_ext::nonzero;
use poise::serenity_prelude::*;
use poise::{CreateReply, FrameworkContext};
use shuttle_runtime::SecretStore;
use std::collections::HashMap;
use std::mem;
use std::num::NonZeroU64;
use std::ops::DerefMut;
use std::sync::{Arc, LazyLock};
use tokio::join;
use tokio::sync::RwLock;
use tokio::task::JoinSet;
use tracing::log::warn;

// the comment sets the description
#[poise::command(slash_command)]
/// Use this to check if the bot is alive
async fn ping(ctx: poise::Context<'_, (), Error>) -> Result<(), Error> {
    let response = CreateReply::default().content("Pong").ephemeral(true);
    ctx.send(response).await?;
    Ok(())
}

const SEND_SERVER: NonZeroU64 = nonzero!(1294665730368737373_u64);
const RECEIVE_SERVER: NonZeroU64 = nonzero!(1294665933704658975_u64);
#[derive(Debug, Clone, Default)]
struct ChannelMaps {
    channel_map: HashMap<ChannelId, Vec<ChannelId>>,
    webhook_map: HashMap<ChannelId, Webhook>,
}
static MAP: LazyLock<RwLock<ChannelMaps>> = LazyLock::new(|| RwLock::new(ChannelMaps::default()));
#[shuttle_runtime::main]
async fn serenity(
    #[shuttle_runtime::Secrets] secret_store: SecretStore,
) -> shuttle_serenity::ShuttleSerenity {
    // Get the discord token set in `Secrets.toml`
    let discord_token = secret_store
        .get("DISCORD_TOKEN")
        .context("'DISCORD_TOKEN' was not found")?;
    let framework = poise::Framework::builder()
        .options(poise::FrameworkOptions {
            commands: vec![ping()],
            event_handler: |ctx, event, framework, data| {
                Box::pin(event_handler(ctx, event, framework, data))
            },
            ..Default::default()
        })
        .setup(move |ctx, _ready, framework| {
            Box::pin(async move {
                poise::builtins::register_globally(ctx, &framework.options().commands).await?;
                Ok(())
            })
        })
        .build();

    let intents =
        GatewayIntents::MESSAGE_CONTENT | GatewayIntents::GUILDS | GatewayIntents::GUILD_MESSAGES;
    let client = ClientBuilder::new(discord_token, intents)
        .framework(framework)
        .await
        .expect("Error creating client");

    Ok((client).into())
}
async fn event_handler<'a>(
    ctx: &'a Context,
    event: &'a FullEvent,
    _fw_ctx: FrameworkContext<'a, (), Error>,
    _: &'a (),
) -> Result<(), Error> {
    match event {
        FullEvent::Ready {
            data_about_bot: bot_data,
            ..
        } => {
            println!("Logged in as {}", bot_data.user.name);
            ready(ctx).await;
        }
        FullEvent::Message {
            new_message: message,
        } => {
            handle_message(ctx, message.clone()).await;
        }
        FullEvent::ChannelCreate { channel: _channel } => {
            ready(ctx).await;
        }
        _ => {}
    }
    Ok(())
}

async fn handle_message(ctx: &Context, message: Message) {
    if let Some(send_to) = MAP.read().await.channel_map.get(&message.channel_id) {
        for x in send_to {
            send_message(&ctx, &message, *x).await;
        }
    }
}

async fn send_message(ctx: &&Context, message: &Message, x: ChannelId) {
    let message = message.clone();
    let author = message.author;
    let builder = ExecuteWebhook::new()
        .content(&message.content)
        .username(&author.name)
        .avatar_url(
            author
                .avatar_url()
                .unwrap_or_else(|| "https://cdn.discordapp.com/embed/avatars/1.png".into()),
        )
        .embeds(
            message
                .embeds
                .into_iter()
                .map(|x| x.into())
                .collect::<Vec<CreateEmbed>>(),
        )
        .files(handle_attachment(ctx, &message.attachments).await);
    let webhook_map = &MAP.read().await.webhook_map;
    let webhook = webhook_map.get(&x).expect("Failed to get webhook from map");
    webhook
        .execute(&ctx.http, false, builder)
        .await
        .expect("Failed to send message");
}

async fn handle_attachment(
    ctx: &Context,
    attachments: &[Attachment],
) -> impl IntoIterator<Item = CreateAttachment> {
    let mut set = JoinSet::new();

    attachments.iter().for_each(|x| {
        set.spawn(attachment_from_url(ctx.http.clone(), x.url.clone()));
    });

    let create = set.join_all().await;
    create.into_iter().filter_map(|x| match x {
        Ok(a) => Some(a),
        Err(err) => {
            warn!("Error proxying attachment {:?}", err);
            None
        }
    })
}

async fn ready(ctx: &Context) {
    let channel_map = discover_channels(&ctx.http, SEND_SERVER.into(), RECEIVE_SERVER.into()).await;
    let webhook_map = init_webhooks(&ctx.http, &channel_map).await;
    let mut guard = MAP.write().await;
    let _ = mem::replace(
        guard.deref_mut(),
        ChannelMaps {
            channel_map,
            webhook_map,
        },
    );
}
async fn discover_channels(
    http: &Arc<Http>,
    sender_id: GuildId,
    receiver_id: GuildId,
) -> HashMap<ChannelId, Vec<ChannelId>> {
    let (send_channels, receive_channels) =
        join!(sender_id.channels(http), receiver_id.channels(http));
    let sender = send_channels.expect("could not discover channels in sender");
    let receiver = receive_channels.expect("could not discover channels in receiver");
    let mut new_map = HashMap::new();
    for (sender_id, sender_channel) in sender
        .into_iter()
        .filter(|(_, a)| a.kind != ChannelType::Category)
    {
        match receiver
            .values()
            /* needed so that you don't get channels that will cause errors in webhook creation */
            .filter(|x| x.kind != ChannelType::Category)
            .find(|x| x.name().contains(&sender_id.to_string()))
        {
            Some(x) => {
                new_map.insert(sender_id, vec![x.into()]);
            }
            None => {
                // No channel exists in target server so create one
                let channel =
                    CreateChannel::new(format!("{}-{}", sender_channel.name(), sender_id))
                        .kind(sender_channel.kind);
                let channel = match receiver_id.create_channel(http, channel).await {
                    Ok(x) => x,
                    Err(e) => match e {
                        Error::Model(ModelError::InvalidPermissions {
                            required,
                            present: _,
                        }) => {
                            panic!("need {required} in receiving server")
                        }
                        e => panic!("{:?}", e),
                    },
                };
                new_map.insert(sender_id, vec![channel.id]);
            }
        }
    }
    new_map
}
async fn init_webhooks(
    http: &Arc<Http>,
    channel_map: &HashMap<ChannelId, Vec<ChannelId>>,
) -> HashMap<ChannelId, Webhook> {
    let mut map: HashMap<ChannelId, Webhook> = HashMap::with_capacity(channel_map.len());
    for (_, channels) in channel_map.iter() {
        for &channel_id in channels {
            let webhooks = channel_id.webhooks(&http).await.unwrap_or_else(|x| {
                panic!("Could not fetch webhooks for channel {channel_id} {channel_map:?}: {x:?}")
            });
            let webhook = match webhooks.into_iter().find(|x| x.token.is_some()) {
                Some(x) => x,
                None => {
                    let x = CreateWebhook::new("Forward Bot");
                    channel_id
                        .create_webhook(&http, x)
                        .await
                        .expect("Tried to create webhook for channel without one, but failed")
                }
            };
            map.insert(channel_id, webhook);
        }
    }
    map
}
pub async fn attachment_from_url(http: impl AsRef<Http>, url: String) -> Result<CreateAttachment> {
    CreateAttachment::url(http, &url).await
}
