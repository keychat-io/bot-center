#[macro_use]
extern crate anyhow;
#[macro_use]
extern crate serde;
#[macro_use]
extern crate tracing;

mod config;
use config::{Config, Opts};
mod db;
use db::LitePool;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    use clap::Parser;
    let opts = Opts::parse();

    let fmt = tracing_subscriber::fmt().with_line_number(true);
    if std::env::var("RUST_LOG").is_ok() {
        fmt.init();
    } else {
        let max = opts.log();
        fmt.with_max_level(max).init()
    }

    // let addr = "0.0.0.0:50051".parse().unwrap();
    let config = match opts.parse_config() {
        Ok(c) => c,
        Err(e) => {
            error!("config parse failed: {}", e);
            return Err(e.into());
        }
    };
    let db = LitePool::open(&config.database).await?;

    // let my_keys = Keys::generate();
    // println!("{}: {}", my_keys.secret_key().unwrap().to_secret_hex(), my_keys.public_key().to_bech32().unwrap());
    // return Ok(());

    let (bs, _bc) = broadcast::channel(1000);
    let clients = DashMap::new();
    let secrets = dotenvy::var("BOT_CENTER_SECRETS")?;
    for (i, secret) in secrets
        .split(";")
        .map(|s| s.trim())
        .filter(|s| s.len() >= 1)
        .enumerate()
    {
        let my_keys = Keys::parse(secret)?;

        let addr = config.listen;
        let pubkey_bech32 = my_keys.public_key().to_bech32()?;
        info!(
            "{} {}\npubkey: {} {}\n",
            i,
            addr,
            pubkey_bech32,
            my_keys.public_key().to_hex(),
        );

        let client = connect(&config, &my_keys).await?;
        let meta = client
            .metadata(my_keys.public_key())
            // .fetch_metadata(
            //     my_keys.public_key(),
            //     Some(Duration::from_millis(config.timeout_ms)),
            // )
            .await;
        debug!("metadata: {:?}", meta);
        match meta {
            Ok(md) => {
                info!("fetch_metadata ok: {}", serde_json::to_string(&md)?);
            }
            Err(nostr_sdk::client::Error::MetadataNotFound) => {
                warn!("fetch_metadata failed: {}", 404);
                // let metadata = Metadata::new()
                //     .name("username")
                //     .display_name("My Username")
                //     .about("Description")
                //     .picture(Url::parse("https://example.com/avatar.png").unwrap())
                //     .nip05("username@example.com");

                // let setid = client.set_metadata(&metadata).await?;
                // info!("set_metadata: {:?}", setid);
            }
            Err(e) => {
                error!("fetch_metadata failed: {}", e);
                // return Err(e.into());
            }
        }

        clients.insert(my_keys.public_key().to_hex(), client);
    }

    let state = State {
        config,
        clients,
        events: bs.into(),
        db,
    };
    for kv in state.clients.iter() {
        let k = kv.key().clone();
        let c = kv.value().clone();
        let s = state.clone();
        let fut = async move {
            let sleep = std::time::Duration::from_millis(1000);
            loop {
                let ts = s.db.get_events_ts_max(&k).await.unwrap() / 1000;
                let subscription = Filter::new()
                    // .pubkeys(vec![k.parse().unwrap()])
                    .custom_tag(SingleLetterTag::lowercase(Alphabet::P), vec![k.clone()])
                    // .kinds([Kind::Custom(4), Kind::Custom(1059), Kind::Custom(0)])
                    .kinds([Kind::Custom(4), Kind::Custom(1059)])
                    // .since(0.into());
                    .since(ts.into());
                // .since(Timestamp::now());

                // let subscriber = c
                //     .stream_events_of(vec![subscription], Duration::from_secs(600).into())
                //     .await;

                // can't without close?
                let opts =
                    FilterOptions::WaitDurationAfterEOSE(Duration::from_secs(3600 * 24 * 30));

                let opts = SubscribeAutoCloseOptions::default().filter(opts);
                let res = c
                    .subscribe_with_id(SubscriptionId::new("s0"), vec![subscription], Some(opts))
                    .await;
                match res {
                    Ok(s) => info!("{} subscribe_with_id {:?} ok: {}", k, s.val, ""),
                    Err(e) => {
                        error!("{} subscribe_with_id s0 failed: {}", k, e);
                    }
                }

                let k2 = k.clone();
                let c2 = c.clone();
                let s = s.clone();
                let h = move |rn: RelayPoolNotification| {
                    let k = k2.clone();
                    let s = s.clone();
                    let c = c2.clone();

                    async move {
                        match rn {
                            RelayPoolNotification::Event {
                                subscription_id: ks,
                                relay_url: _,
                                event,
                            } => {
                                info!(
                                    "{} stream_event ok: {} {} {} {}\n{:?} {:?}",
                                    k,
                                    ks,
                                    event.created_at,
                                    event.kind,
                                    event.id,
                                    event.tags,
                                    event.content
                                );

                                let mut wrap = EventMsg::default();
                                match event.kind.as_u16() {
                                    4 => {
                                        // let msg = keychat_rust_ffi_plugin::api_nostr::decrypt(
                                        //     event.author().to_hex(),
                                        //     k.clone(),
                                        //     event.content().to_owned(),
                                        // );
                                        let s = c.signer().await.unwrap();
                                        let msg =
                                            s.nip04_decrypt(event.author(), event.content()).await;
                                        match msg {
                                            Ok(m) => {
                                                // double 04: ["EVENT", {}]
                                                if let Ok((_, m)) =
                                                    serde_json::from_str::<(String, Event)>(&m)
                                                {
                                                    if m.kind.as_u16() == 4 {
                                                        if let Ok(m) =  s.nip04_decrypt(m.author(), m.content()).await.map_err(|e|{
                                                            error!(
                                                                "{} stream_event 04.04 decrypt failed: {} {} {} {} {}",
                                                                k, ks, event.created_at, event.kind, event.id, e
                                                            );
                                                        }){
                                                            wrap.content.replace(m);
                                                        }
                                                    }
                                                }
                                                if wrap.content.is_none() {
                                                    wrap.content.replace(m);
                                                }
                                            }
                                            Err(e) => {
                                                error!(
                                                    "{} stream_event 04 decrypt failed: {} {} {} {} {}",
                                                    k, ks, event.created_at, event.kind, event.id, e
                                                );
                                            }
                                        }
                                    }
                                    1059 => {
                                        let msg = c.unwrap_gift_wrap(&event).await;
                                        match msg {
                                            Ok(m) => {
                                                wrap.from = m.sender.to_hex();
                                                wrap.content.replace(m.rumor.content);
                                                wrap.ts = m.rumor.created_at.as_u64() * 1000;
                                            }
                                            Err(e) => {
                                                error!(
                                                    "{} stream_event 1059 decrypt failed: {} {} {} {} {}",
                                                    k, ks, event.created_at, event.kind, event.id, e
                                                );
                                            }
                                        }
                                    }
                                    _ => return Ok(false),
                                }

                                let id = event.id.clone();
                                if let Some(c) = &mut wrap.content {
                                    info!("{} event {} content: {:?}", k, id, c);

                                    // let dec = base64_simd::STANDARD_NO_PAD.decode_to_vec(&c);
                                    // info!("{} event {} decode_base64: {:?}", k, id, dec);
                                    // if let Ok(bs) = dec {
                                    //     if let Ok(s) = String::from_utf8(bs) {
                                    //         info!("{} event {} decode_as_str ok: {}", k, id, s);
                                    //         *c = s;
                                    //     }
                                    // }
                                }

                                wrap.to = k.clone();
                                wrap.id = id.to_hex();
                                wrap.kind = event.kind().as_u16();
                                if wrap.from.is_empty() {
                                    wrap.from = event.author().to_hex();
                                }
                                if wrap.ts == 0 {
                                    wrap.ts = event.created_at.as_u64() * 1000;
                                }

                                let insert = s.db.insert_event(&wrap).await;
                                info!("{} stream_events_of {} insert: {:?}", k, id, insert);
                                if insert.is_ok() {
                                    // wrap.event.replace(event);
                                    s.events
                                        .send(wrap)
                                        .map_err(|e| {
                                            error!(
                                                "{} stream_events_of {} broadcast failed: {}",
                                                k, id, e
                                            )
                                        })
                                        .ok();
                                }
                            }
                            RelayPoolNotification::Message {
                                message,
                                relay_url: _,
                            } => {
                                if let RelayMessage::Event {
                                    subscription_id,
                                    event,
                                } = message
                                {
                                    debug!("{} {} {:?}", k, subscription_id, event);
                                } else {
                                    info!("{} relay message: {:?}", k, message);
                                }
                            }
                            RelayPoolNotification::RelayStatus {
                                status,
                                relay_url: _,
                            } => info!("{} relay status: {}", k, status,),
                            RelayPoolNotification::Shutdown => {}
                        }
                        Ok(false)
                    }
                };
                // h -> exit
                match c.handle_notifications(h).await {
                    Ok(()) => info!("{} handle_notifications ok: {:?}", k, ()),
                    Err(e) => error!("{} handle_notifications failed: {}", k, e),
                };
                tokio::time::sleep(sleep).await;
            }
        };
        tokio::spawn(fut);
    }

    // build our application with some routes
    let app = Router::new()
        .route("/metadata/:key", axum::routing::post(post_metadata))
        .route("/event/from/:from/to/:to", axum::routing::post(post_event))
        .route("/ws", any(ws_handler))
        .with_state(state.clone())
        .layer(
            TraceLayer::new_for_http()
                .make_span_with(DefaultMakeSpan::default().include_headers(true)),
        );

    // run it with hyper
    let listener = tokio::net::TcpListener::bind(state.config.listen)
        .await
        .unwrap();
    info!("listening on {}", listener.local_addr().unwrap());
    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .await
    .unwrap();

    Ok(())
}

use nostr_sdk::prelude::*;
use std::time::Duration;
async fn connect(config: &Config, keys: &Keys) -> anyhow::Result<Client> {
    let opts = nostr_sdk::client::options::Options::new()
        // .autoconnect(true)
        .connection_timeout(Some(Duration::from_millis(6000)));
    let client = Client::with_opts(keys, opts);

    // Add relays
    for relay in &config.relays {
        client.add_relay(relay).await?;
        // add_relay_with_opts
    }

    // Connect to relays
    client.connect().await;

    Ok(client)
}

#[derive(Clone)]
pub struct State {
    pub(crate) config: Config,
    pub(crate) clients: DashMap<String, Client>,
    pub(crate) events: broadcast::Sender<EventMsg>,
    pub(crate) db: LitePool,
}

#[derive(Clone, Default, Debug, Serialize, Deserialize)]
pub struct EventMsg {
    id: String,
    from: String,
    to: String,
    ts: u64,
    kind: u16,
    content: Option<String>,
    #[serde(skip)]
    comfirmed: bool,
    // event: Option<Box<Event>>,
    // error: Option<Arc<anyhow::Error>>,
}

use axum::{
    extract::{
        ws::{Message, WebSocket, WebSocketUpgrade},
        ConnectInfo, Path, State as AxumState,
    },
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    routing::any,
    Json, Router,
};

use dashmap::DashMap;
use tokio::sync::broadcast;

use std::net::SocketAddr;
use tower_http::trace::{DefaultMakeSpan, TraceLayer};

//allows to split the websocket stream into separate TX and RX branches
use futures::{sink::SinkExt, stream::StreamExt};

// async fn get_metadata(
//     ConnectInfo(_sa): ConnectInfo<SocketAddr>,
//     AxumState(state): AxumState<State>,
//     Path(key): Path<String>,
// ) -> Json<Value> {
//     Json(js)
// }

async fn post_metadata(
    sa: ConnectInfo<SocketAddr>,
    state: AxumState<State>,
    Path(key): Path<String>,
    _header: HeaderMap,
    body: String,
) -> impl IntoResponse {
    post_event(sa, state, Path((key, String::new())), _header, body).await
}

async fn post_event(
    ConnectInfo(sa): ConnectInfo<SocketAddr>,
    AxumState(state): AxumState<State>,
    Path((from, to)): Path<(String, String)>,
    _header: HeaderMap,
    body: String,
) -> impl IntoResponse {
    info!(
        "{} post_event {}->{} {}:\n{:?}",
        sa,
        from,
        to,
        body.len(),
        body
    );

    let f = |s| axum::response::Response::new(s);
    let mut code = 200;
    match post_event_(sa, &state, &from, &to, &body, &mut code).await {
        Ok(body) => {
            let js = serde_json::to_string(&body).unwrap();
            f(js)
        }
        Err(e) => {
            warn!("{} post_event {}->{} failed: {}", sa, from, to, e,);

            let mut resp = f(e.to_string());
            *resp.status_mut() = StatusCode::from_u16(code).unwrap();
            resp
        }
    }
}

async fn post_event_(
    _sa: SocketAddr,
    state: &State,
    key: &str,
    user: &str,
    body: &String,
    code: &mut u16,
) -> anyhow::Result<WsMessage> {
    if key.len() < 1 {
        bail!("key from empty");
    }
    key.parse::<PublicKey>().inspect_err(|_e| *code = 400)?;

    let client = state.clients.get(key).map(|c| c.clone()).ok_or_else(|| {
        *code = 404;
        anyhow!("not found the key from")
    })?;

    let tag = SingleLetterTag::lowercase(Alphabet::P);
    // let typo = js.get("type").and_then(|v| v.as_str()).unwrap_or_default();
    if user.is_empty() {
        let js = serde_json::from_str::<Value>(&body).inspect_err(|_e| *code = 400)?;
        let metadata: Metadata = serde_json::from_value(js).inspect_err(|_e| *code = 400)?;
        let builder = EventBuilder::metadata(&metadata).add_tags(vec![Tag::custom(
            TagKind::SingleLetter(tag.clone()),
            vec![key.to_string()],
        )]);

        let sent = client
            .send_event_builder(builder)
            .await
            .inspect_err(|_e| *code = 500)?;
        let wm = WsMessage::new(200).data(sent.val.to_hex());
        Ok(wm)
    } else {
        if user.len() < 1 {
            bail!("key to missing");
        }
        user.parse::<PublicKey>().inspect_err(|_e| *code = 400)?;
        let s = client.signer().await.unwrap();
        // let ks = if let NostrSigner::Keys(ks) = &s {
        //     ks
        // } else {
        //     unreachable!()
        // };

        let msg = s
            .nip04_encrypt(user.parse().inspect_err(|_e| *code = 400)?, body)
            .await
            .inspect_err(|_e| *code = 500)?;

        // todo?: double nip04_encrypt
        let builder = EventBuilder::new(
            Kind::EncryptedDirectMessage,
            msg,
            vec![Tag::custom(
                TagKind::SingleLetter(tag.clone()),
                vec![user.to_string()],
            )],
        );

        // let enc = keychat_rust_ffi_plugin::api_nostr::encrypt(sender_keys, receiver_pubkey, content)

        // sig
        let resp = client
            .send_event_builder(builder)
            .await
            .inspect_err(|_e| *code = 500)?;

        let wm = WsMessage::new(200).data(resp.val.to_hex());
        Ok(wm)
    }
}

async fn ws_handler(
    ws: WebSocketUpgrade,
    headers: HeaderMap,
    ConnectInfo(sa): ConnectInfo<SocketAddr>,
    AxumState(state): AxumState<State>,
) -> impl IntoResponse {
    // finalize the upgrade process by returning upgrade callback.
    // we can customize the callback by sending additional info such as address.
    ws.on_upgrade(move |socket| handle_socket_wrap(socket, sa, state, headers))
}

async fn handle_socket_wrap(socket: WebSocket, who: SocketAddr, state: State, headers: HeaderMap) {
    let user_agent = headers
        .get("user-agent")
        .and_then(|s| s.to_str().ok())
        .unwrap_or("<none>");

    info!("ws-{who} connect: {}", user_agent);

    match handle_socket(socket, who, state).await {
        Ok(()) => info!("ws-{who} eof",),
        Err(e) => warn!("ws-{who} eof with: {}", e),
    }
}

#[derive(Clone, Default, Serialize, Deserialize)]
pub struct WsMessage {
    code: u16,
    error: Option<String>,
    data: Option<String>,
}

impl WsMessage {
    fn new(code: u16) -> Self {
        let mut it = Self::default();
        it.code = code;
        it
    }
    fn code(mut self, code: u16) -> Self {
        self.code = code;
        self
    }
    fn error(mut self, error: impl Into<String>) -> Self {
        self.error = Some(error.into());
        self
    }
    fn data(mut self, data: impl Into<String>) -> Self {
        self.data = Some(data.into());
        self
    }
}

async fn handle_socket(mut socket: WebSocket, who: SocketAddr, state: State) -> anyhow::Result<()> {
    let first = socket
        .recv()
        .await
        .ok_or_else(|| format_err!("ws read first message"))??;
    let subscribe = if let Message::Text(text) = first {
        text
    } else {
        bail!("unknown message: {:?}", first);
    };

    let pubkeys: Vec<_> = subscribe
        .split(";")
        .map(|s| s.trim())
        .filter(|s| s.len() >= 1)
        .collect();
    info!("ws-{who} subcribe: {:?}", pubkeys);

    let mut metas = vec![];
    for p in &pubkeys {
        let c = state.clients.get(*p).map(|c| c.clone());
        if c.is_none() {
            let wm = WsMessage::default().code(404);
            metas.push(wm);
            continue;
        };

        let filter: Filter = Filter::new()
            .author(public_key)
            .kind(Kind::Metadata)
            .limit(1);
        let events: Vec<Event> = self.get_events_of(vec![filter], None).await?; // TODO: add timeout?
        match events.first() {
            Some(event) => Ok(Metadata::from_json(event.content())?),
            None => Err(Error::MetadataNotFound),
        }

        let filter = Filter::new()
            .custom_tag(SingleLetterTag::lowercase(Alphabet::P), vec![p.clone()])
            .kinds([Kind::Custom(0)])
            .limit(1);
        let events = c.unwrap().get_events_of(vec![filter], None).await;
        // let meta = c.unwrap().metadata(p.parse()?).await;
        match events.and_then(|s|s.first().ok_or_else(|nostr_sdk::) {
            Ok(m) => {
                let js = serde_json::to_string(&m)?;
                metas.push(WsMessage::new(200).data(js));
            }
            Err(e) => {
                metas.push(WsMessage::new(500).error(e.to_string()));
            }
        }
    }

    let (mut sender, mut receiver) = socket.split();
    let js = serde_json::to_string(&metas)?;
    sender.send(Message::Text(js)).await?;
    if metas.iter().all(|m| m.code != 200) {
        bail!("subcribe keys not inited");
    }

    let events = state.db.get_events(&pubkeys).await?;
    let (mp, mut sc) = broadcast::channel(std::cmp::max(events.len(), 1));
    for event in events {
        mp.send(event).unwrap();
    }

    let mut events = Some(state.events.subscribe());
    loop {
        if sc.is_empty() && events.is_some() {
            sc = events.take().unwrap();
        }

        tokio::select! {
            event = sc.recv()  => {
                match event {
                    Ok(a) => {
                        info!("ws-{who} recv broadcasted events: {}", a.id);
                        if pubkeys.contains(&a.to.as_ref()) {
                            let js = serde_json::to_string(&a).unwrap();
                            let res = sender.send(Message::Text(js)).await;
                            info!("ws-{who} send {}: {:?}", a.id, res);
                            res?;
                        }
                    }
                    Err(a) => error!("ws-{who} recv broadcasted events failed: {a:?}")
                }
            },
            msg = receiver.next() => {
                info!("ws-{who} message: {:?}", msg);
                if msg.is_none() {
                    break;
                }

                let msg = msg.unwrap()?;
                let msg = if let Message::Text(text) = msg{
                    text
                } else {
                    bail!("unknown message: {:?}", msg);
                };
                let ids: Vec<_> = msg
                .split(";")
                .map(|s| s.trim())
                .filter(|s| s.len() >= 1)
                .collect();

                let confirm = state.db.update_event(&ids, true).await?;
                info!("ws-{who} message confirm: {:?} {}", confirm, msg);
            }
        }
    }

    // use axum::extract::ws::CloseFrame;
    // sender
    //     .send(Message::Close(Some(CloseFrame {
    //         code: axum::extract::ws::close_code::NORMAL,
    //         reason: Cow::from("Goodbye"),
    //     })))
    //     .await;
    Ok(())
}
