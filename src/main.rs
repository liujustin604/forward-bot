use anyhow::Context as _;
use heapless::LinearMap;
use poise::serenity_prelude::*;
use poise::{CreateReply, FrameworkContext};
use shuttle_runtime::SecretStore;
use std::num::NonZeroU64;
use std::sync::{Arc, LazyLock};
use tokio::sync::OnceCell;
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
const fn nonzero(x: u64) -> NonZeroU64 {
    match NonZeroU64::new(x) {
        Some(x) => x,
        None => panic!("tried to unwrap a none"),
    }
}
const ENTRIES: usize = 1;
static CHANNEL_MAP: LazyLock<heapless::LinearMap<u64, &[NonZeroU64], 1>> = LazyLock::new(|| {
    let mut map: LinearMap<u64, &[NonZeroU64], ENTRIES> = LinearMap::new();
    // second test general => first test general
    const SENDERS: u64 = 1294665730368737376;
    const RECEIVERS: &'static [NonZeroU64] = &[nonzero(1294665934174294060)];
    let _ = map
        .insert(SENDERS, RECEIVERS)
        .expect("Could not insert pair");
    map
});
static WEBHOOK_MAP: OnceCell<LinearMap<ChannelId, Webhook, ENTRIES>> = OnceCell::const_new();
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
            handle_message(ctx, message).await;
        }
        _ => {}
    }
    Ok(())
}

async fn handle_message(ctx: &Context, message: &Message) {
    if let Some(&send_to) = CHANNEL_MAP.get(&message.channel_id.get()) {
        let map = WEBHOOK_MAP.get().expect("Failed to get webhooks");
        for x in send_to {
            send_message(&ctx, message, map, x).await;
        }
    }
}

async fn send_message(
    ctx: &&Context,
    message: &Message,
    map: &LinearMap<ChannelId, Webhook, 1>,
    x: &NonZeroU64,
) {
    let builder = ExecuteWebhook::new()
        .content(message.content.clone())
        .username(message.author.name.clone())
        .avatar_url(
            message
                .author
                .avatar_url()
                .unwrap_or_else(|| "https://cdn.discordapp.com/embed/avatars/1.png".into()),
        )
        .embeds(
            message
                .embeds
                .iter()
                .cloned()
                .map(|x| x.into())
                .collect::<Vec<CreateEmbed>>(),
        )
        .files(handle_attachment(ctx, message).await);
    let webhook = map
        .get(&ChannelId::from(*x))
        .expect("Failed to get webhook from map");
    webhook
        .execute(&ctx.http, false, builder)
        .await
        .expect("Failed to send message");
}

async fn handle_attachment(
    ctx: &Context,
    message: &Message,
) -> impl IntoIterator<Item = CreateAttachment> {
    let mut set = JoinSet::new();

    message.attachments.iter().cloned().for_each(|x| {
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

async fn ready(_ctx: &Context) {
    init_webhooks(_ctx.http.clone()).await
}
async fn init_webhooks(http: Arc<Http>) {
    let mut map: LinearMap<_, _, ENTRIES> = LinearMap::new();
    for (_, channels) in CHANNEL_MAP.iter() {
        for id in *channels {
            let channel_id = ChannelId::from(*id);
            let webhooks = channel_id
                .webhooks(&http)
                .await
                .expect(&format!("Could not fetch webhooks for channel {id}"));
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
            map.insert(channel_id, webhook)
                .expect("Could not add webhook");
        }
    }
    WEBHOOK_MAP.set(map).expect("Failed to set webhooks");
}
pub async fn attachment_from_url(http: impl AsRef<Http>, url: String) -> Result<CreateAttachment> {
    CreateAttachment::url(http, &url).await
}
