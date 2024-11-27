#[macro_use]
extern crate anyhow;
#[macro_use]
extern crate serde;
#[macro_use]
extern crate tracing;

use keychat_rust_ffi_plugin::{api_cashu, api_nostr};

mod config;
use config::{Config, Opts};
mod db;
use db::{unix_time_ms, LitePool};
mod signal;

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

    if opts.generate {
        let my_keys = Keys::generate();
        println!(
            "{}: {}",
            my_keys.secret_key().to_secret_hex(),
            my_keys.public_key().to_hex()
        );
        println!(
            "{}: {}",
            my_keys.secret_key().to_bech32().unwrap(),
            my_keys.public_key().to_bech32().unwrap(),
        );

        if true {
            // let (prik, pubk) = api_signal::generate_signal_ids()?;
            let keys = signal::generate_signal_keypair(&my_keys.secret_key().to_secret_hex())?;
            println!("{}: {}", keys.prikey, keys.pubkey);
        }

        return Ok(());
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

    signal::init_db(config.database_signal.to_string()).await?;

    api_cashu::init_db(config.cashu.database.clone(), None, false).await?;
    let mints = api_cashu::init_cashu(64).await?;
    for mint in config
        .cashu
        .mints
        .iter()
        .filter(|m| !mints.iter().any(|i| i.url == m.as_str()))
    {
        api_cashu::add_mint(mint.to_string()).await?;
    }
    tokio::spawn(async {
        let mut interval = tokio::time::interval(Duration::from_secs(600));
        loop {
            interval.tick().await;
            let res = api_cashu::check_pending().await;
            info!("api_cashu::check_pending: {:?}", res);
            if let Ok((up, all)) = res {
                let ts = (api_cashu::cashu_wallet::cashu::util::unix_time() - 600) * 1000;
                if up > 0 {
                    let res =
                        api_cashu::remove_transactions(ts, api_cashu::TransactionStatus::Success)
                            .await;
                    info!("api_cashu::remove_transactions.success: {:?}", res);
                }

                if all > up {
                    let txs = api_cashu::get_cashu_pending_transactions().await;
                    if let Ok(txs) = txs {
                        for tx in txs {
                            if tx.time < ts {
                                let res = api_cashu::receive_token(tx.token.to_string()).await;
                                info!(
                                    "api_cashu::receive_token.recycle {}: {:?}",
                                    tx.amount,
                                    res.map(|txs| txs.iter().map(|tx| tx.amount()).sum::<u64>())
                                );
                            }
                        }
                    }
                }
            }
        }
    });

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
        let keypair = signal::generate_signal_keypair(&my_keys.secret_key().to_secret_hex())?;
        signal::init_keypair(&keypair).await?;

        let addr = config.listen;
        let pubkey_bech32 = my_keys.public_key().to_bech32()?;
        info!(
            "{} {}\npubkey: {} {}\nsignalid: {}",
            i,
            addr,
            pubkey_bech32,
            my_keys.public_key().to_hex(),
            keypair.pubkey
        );

        let client = connect(&config, &my_keys).await?;
        let meta = client
            // .metadata(my_keys.public_key())
            .fetch_metadata(
                my_keys.public_key(),
                Some(Duration::from_millis(config.timeout_ms)),
            )
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

        let client = ClientWithKeys {
            client,
            nostr: my_keys,
            signal: keypair,
            // lock: Mutex::const_new(()),
        };

        let key = client.nostr.public_key().to_hex();
        clients.insert(key, client.into());
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
                    .client
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
                let instant = std::time::Instant::now();
                let alive = std::sync::Arc::new(AtomicU64::new(0));
                let alive2 = alive.clone();
                let h = move |rn: RelayPoolNotification| {
                    let k = k2.clone();
                    let s = s.clone();
                    let c = c2.clone();
                    let alive = alive2.clone();

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
                                let mut nip04_double = false;
                                match event.kind.as_u16() {
                                    4 => {
                                        let tagsp2 = event
                                            .tags
                                            .iter()
                                            .filter(|t| {
                                                let t = t.as_slice();
                                                t.len() > 1 && t[0] == "p"
                                            })
                                            .map(|t| {
                                                t.as_slice().iter().skip(1).take_while(|s| {
                                                    **s != k && PublicKey::from_hex(s).is_ok()
                                                })
                                            })
                                            .flatten()
                                            .next();

                                        if let Some(p2) = tagsp2 {
                                            let res = signal::decrypt_msg(
                                                &event.content,
                                                &k,
                                                &p2,
                                                &c.signal,
                                                &s.db,
                                            )
                                            .await;

                                            match res {
                                                Ok((m, from)) => {
                                                    wrap.from = from;
                                                    wrap.content.replace(m);
                                                }
                                                Err(e) => {
                                                    error!(
                                                        "{} stream_event 04 decrypt as signal->{} failed: {} {} {} {} {}",
                                                        k, p2, ks, event.created_at, event.kind, event.id, e
                                                    );
                                                }
                                            }
                                        } else {
                                            let s = c.client.signer().await.unwrap();
                                            let msg = s
                                                .nip04_decrypt(&event.pubkey, &event.content)
                                                .await;
                                            match msg {
                                                Ok(m) => {
                                                    // double 04: ["EVENT", {}]
                                                    if let Ok((_, e)) =
                                                        serde_json::from_str::<(String, Event)>(&m)
                                                    {
                                                        if e.kind.as_u16() == 4 {
                                                            if let Ok(m) =  s.nip04_decrypt(&e.pubkey, &e.content).await.map_err(|e|{
                                                                error!(
                                                                    "{} stream_event 04.04 decrypt failed: {} {} {} {} {}",
                                                                    k, ks, event.created_at, event.kind, event.id, e
                                                                );
                                                            }){
                                                                wrap.from = e.pubkey.to_hex();
                                                                wrap.content.replace(m);
                                                                nip04_double = true;
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
                                    }
                                    1059 => {
                                        let msg = c.client.unwrap_gift_wrap(&event).await;
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
                                wrap.kind = event.kind.as_u16();
                                if wrap.from.is_empty() {
                                    wrap.from = event.pubkey.to_hex();
                                }
                                if wrap.ts == 0 {
                                    wrap.ts = event.created_at.as_u64() * 1000;
                                }

                                let mut handshake = None;
                                if nip04_double || wrap.kind == 1059 {
                                    match signal::try_decode_handshake(&mut wrap).await {
                                        Ok(m) => {
                                            handshake.replace(m);
                                        }
                                        Err(e) => {
                                            error!(
                                                "{} event {} signal::try_decode_handshake failed: {}",
                                                k, id, e
                                            )
                                        }
                                    }
                                }

                                let insert = s.db.insert_event(&wrap).await;
                                info!(
                                    "{} stream_events_of {}-{} insert: {:?}",
                                    k, id, nip04_double, insert
                                );
                                if insert.is_ok() {
                                    if let Some((h, n)) = handshake {
                                        signal::handle_handshake(
                                            &k, &wrap.from, h, n, &c.signal, &s.db,
                                        )
                                        .await
                                        .map_err(|e| {
                                            error!(
                                                "{} event {} signal::handle_handshake failed: {}",
                                                k, id, e
                                            )
                                        })
                                        .ok();
                                    }

                                    if !wrap.comfirmed {
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
                            }
                            RelayPoolNotification::Message { message, relay_url } => {
                                match message {
                                    RelayMessage::Event {
                                        subscription_id,
                                        event,
                                    } => {
                                        debug!(
                                            "{} {} {} {:?}",
                                            k, relay_url, subscription_id, event
                                        );
                                    }
                                    RelayMessage::Ok {
                                        event_id,
                                        status,
                                        message,
                                    } => {
                                        info!(
                                            "{} {} {} Ok {} {}",
                                            k,
                                            event_id.to_hex(),
                                            relay_url,
                                            status,
                                            message
                                        );
                                    }
                                    others => {
                                        info!("{} relay {} message: {:?}", k, relay_url, others)
                                    }
                                }
                                alive.store(
                                    instant.elapsed().as_secs(),
                                    std::sync::atomic::Ordering::Release,
                                );
                            }
                            RelayPoolNotification::RelayStatus { status, relay_url } => {
                                info!("{} relay {} status: {}", k, relay_url, status);
                                if status == RelayStatus::Terminated {
                                    return Ok(true);
                                }
                            }
                            RelayPoolNotification::Authenticated { relay_url } => {
                                info!("{} relay {} Authenticated", k, relay_url)
                            }
                            RelayPoolNotification::Shutdown => {
                                return Ok(true);
                            }
                        }
                        Ok(false)
                    }
                };

                let c2 = c.clone();
                let fut = tokio::spawn(async move { c2.client.handle_notifications(h).await });
                let mut fut = std::pin::pin!(fut);
                let mut interval = tokio::time::interval(Duration::from_secs(60));
                loop {
                    tokio::select! {
                        res = &mut fut =>  {
                                match res.unwrap() {
                                    Ok(()) => info!("{} handle_notifications ok: {:?}", k, ()),
                                    Err(e) => error!("{} handle_notifications failed: {}", k, e),
                                };
                                break;
                        }
                        _ = interval.tick() => {
                            let meta = c.client.fetch_metadata(k.parse().unwrap(), Some(Duration::from_secs(5))).await;
                            match meta {
                                Ok(md) => {
                                    info!("{} keepalive.fetch_metadata ok: {:?}", k, md.custom.len());
                                }
                                Err(nostr_sdk::client::Error::MetadataNotFound) => {
                                    warn!("{} keepalive.fetch_metadata 404: {}", k, 404);
                                }
                                Err(e) => {
                                    error!("{} keepalive.fetch_metadata failed, reconnect: {}", k, e);
                                    break;
                                }
                            }

                            // wait for fetch_metadata's EndOfStoredEvents
                            tokio::time::sleep(Duration::from_secs(1)).await;
                            if alive.load(std::sync::atomic::Ordering::Acquire) + 10 < instant.elapsed().as_secs() {
                                error!("{} active+10={} < {}, reconnect", k,alive.load(std::sync::atomic::Ordering::Acquire)+10, instant.elapsed().as_secs());
                                break;
                            }
                        }
                    }
                }
                warn!(
                    "{}.disconnect().await: {:?}",
                    k,
                    c.client.disconnect().await
                );
                tokio::time::sleep(sleep).await;
                c.client.connect().await;
                // abort last handle_notifications
                fut.abort();
            }
        };
        tokio::spawn(fut);
    }

    // build our application with some routes
    let app = Router::new()
        .route("/metadata/:key", routing::post(post_metadata))
        .route("/event/from/:from/to/:to", routing::post(post_event))
        .route("/ws", routing::any(ws_handler))
        .route("/balance", routing::get(get_balance))
        .route("/receive", routing::post(post_receive))
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
use std::{future::Future, sync::atomic::AtomicU64, sync::Arc, time::Duration};
async fn connect(config: &Config, keys: &Keys) -> anyhow::Result<Client> {
    let opts = nostr_sdk::client::options::Options::new()
        .autoconnect(true)
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
    pub(crate) clients: DashMap<String, Arc<ClientWithKeys>>,
    pub(crate) events: broadcast::Sender<EventMsg>,
    pub(crate) db: LitePool,
}

use signal::SignalKeys;
// use tokio::sync::Mutex;
pub struct ClientWithKeys {
    pub(crate) client: Client,
    pub(crate) nostr: Keys,
    pub(crate) signal: SignalKeys,
    // pub(crate) lock: Mutex<()>,
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
    routing, Json, Router,
};

use dashmap::DashMap;
use tokio::sync::broadcast;

use std::net::SocketAddr;
use tower_http::trace::{DefaultMakeSpan, TraceLayer};

//allows to split the websocket stream into separate TX and RX branches
use futures::{sink::SinkExt, stream::StreamExt};

async fn get_balance(
    ConnectInfo(_sa): ConnectInfo<SocketAddr>,
    AxumState(_state): AxumState<State>,
) -> Json<WsMessage> {
    let balance = api_cashu::get_balances().await;

    let mut msg = WsMessage::new(200);
    match balance {
        Ok(b) => {
            msg.data.replace(b);
        }
        Err(e) => {
            msg.code = 500;
            msg.error.replace(e.to_string());
        }
    }

    Json(msg)
}

async fn post_receive(
    ConnectInfo(sa): ConnectInfo<SocketAddr>,
    AxumState(state): AxumState<State>,
    _header: HeaderMap,
    body: String,
) -> impl IntoResponse {
    let body = body.trim();
    info!("{} post_receive {} bytes: {}", sa, body.len(), body,);

    let f = |s| axum::response::Response::new(s);
    let mut code = 200;
    match post_receive_(sa, &state, body, &mut code).await {
        Ok(body) => {
            info!("{} post_receive ok: {:?}", sa, body.data);

            let js = serde_json::to_string(&body).unwrap();
            f(js)
        }
        Err(e) => {
            warn!("{} post_receive failed: {}", sa, e);

            let msg = WsMessage::new(code).error(e.to_string());
            let js = serde_json::to_string(&msg).unwrap();

            let mut resp = f(js);
            *resp.status_mut() = StatusCode::from_u16(code).unwrap();
            resp
        }
    }
}

async fn post_receive_(
    _sa: SocketAddr,
    _state: &State,
    body: &str,
    code: &mut u16,
) -> anyhow::Result<WsMessage> {
    // let token = api_cashu::decode_token(body.trim().to_string()).inspect_err(|_e| *code = 400)?;
    let txs = api_cashu::receive_token(body.to_string())
        .await
        .inspect_err(|_e| *code = 400)?;
    let amount = txs.iter().map(|t| t.amount()).sum::<u64>();
    Ok(WsMessage::default().code(200).data(amount.to_string()))
}

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

            let msg = WsMessage::new(code).error(e.to_string());
            let js = serde_json::to_string(&msg).unwrap();

            let mut resp = f(js);
            *resp.status_mut() = StatusCode::from_u16(code).unwrap();
            resp
        }
    }
}

fn event_with_cashu(
    state: &(),
    url: &Url,
    event: Event,
) -> impl Future<Output = Result<ClientMessage, nostr_relay_pool::relay::Error>> {
    let mut msg = None;

    let _state = state.clone();
    let url = url.clone();
    async move {
        use nostr_sdk::nostr::nips::nip11::PaymentMethod;
        use nostr_sdk::nostr::nips::nip11::RelayInformationDocument;
        use std::collections::BTreeMap;
        use tokio::sync::Mutex;
        static RIS: Mutex<BTreeMap<Url, RelayInformationDocument>> =
            Mutex::const_new(BTreeMap::new());

        let mut lock = RIS.lock().await;
        if !lock.contains_key(&url) {
            match RelayInformationDocument::get(url.clone(), None).await {
                Ok(ri) => {
                    lock.insert(url.clone(), ri);
                }
                Err(e) => {
                    error!("get relay info failed {}: {}", url, e)
                }
            }
        }

        // curl -H "Accept: application/nostr+json" https://relay.keychat.io
        if let Some(ri) = lock.get(&url) {
            if let Some(ps) = ri
                .fees
                .as_ref()
                .and_then(|f| f.publication.iter().find(|f| f.method.is_some()))
            {
                let pm = ps.method.as_ref().unwrap();
                if ps.amount > 0 {
                    match pm {
                        PaymentMethod::Cashu { mints } => {
                            match api_cashu::send_stamp(ps.amount as _, mints.to_owned(), None)
                                .await
                            {
                                Ok(tx) => {
                                    info!(
                                        "get cashu stamp for {} ok {}/{} {}: {}",
                                        url,
                                        tx.amount(),
                                        ps.amount,
                                        ps.unit,
                                        tx.content()
                                    );
                                    msg = Some(tx.content().to_string());
                                }
                                Err(e) => {
                                    error!(
                                        "get cashu stamp for {} failed {} {}: {}",
                                        url, ps.amount, ps.unit, e
                                    );
                                }
                            }
                        }
                    }
                }
            }
        }

        Ok(ClientMessage::event_with(event, msg))
    }
    // .boxed()
}

async fn post_event_(
    _sa: SocketAddr,
    state: &State,
    key: &str,
    user: &str,
    body: &String,
    code: &mut u16,
) -> anyhow::Result<WsMessage> {
    PublicKey::from_hex(key).inspect_err(|_e| *code = 400)?;

    let cwk = state.clients.get(key).map(|c| c.clone()).ok_or_else(|| {
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

        let sent = cwk
            .client
            // .send_event_builder(builder)
            .send_event_builder_with(builder, move |url, event| event_with_cashu(&(), url, event))
            .await
            .inspect_err(|_e| *code = 500)?;
        let wm = WsMessage::new(200).data(EventOutput::new(sent).json());
        Ok(wm)
    } else {
        let userpub = PublicKey::from_hex(user).inspect_err(|_e| *code = 400)?;
        let session = state.db.get_session(user, key).await?;

        let mut dst = user.to_string();
        let msg;
        if let Some(session) = session {
            // signal
            let (msgc, myra, keys, _pre) = signal::encrypt_msg(
                body.to_owned(),
                &session.pubkey,
                &session.nostrid,
                &key,
                &cwk.nostr.secret_key().to_secret_hex(),
                &cwk.signal,
                session.onetimekey.len() >= 1,
            )
            .await?;
            debug!(
                "{}->{} myra: {:?}, keys: {}, pre: {:?}",
                key, user, myra, keys, _pre
            );

            if let Some(s) = myra {
                let ra = api_nostr::generate_seed_from_ratchetkey_pair(s)?;
                debug!("{}->{} myra: {}", key, user, ra);

                state
                    .db
                    .insert_receiver(&user, &key, unix_time_ms(), &session.pubkey, &ra)
                    .await
                    .map_err(|e| format_err!("save signal receiver address failed: {}", e))?;
            }

            let mut dest = session.onetimekey.clone();
            let raw_session = signal::get_session(session.pubkey, &cwk.signal)
                .await?
                .ok_or_else(|| format_err!("get raw session none"))?;
            debug!("{}->{} raw_session: {:?}", key, user, raw_session);
            if dest.is_empty() {
                let to = raw_session
                    .bob_address
                    .as_ref()
                    .unwrap_or(&raw_session.address);
                dest = api_nostr::generate_seed_from_ratchetkey_pair(to.to_owned())?;
            }
            msg = base64_simd::STANDARD.encode_to_string(&msgc);
            dst = dest;
        } else {
            // nip04
            let s = cwk.client.signer().await.unwrap();
            msg = s
                .nip04_encrypt(&userpub, body)
                .await
                .inspect_err(|_e| *code = 500)?;
        }

        // todo?: double nip04_encrypt
        let builder = EventBuilder::new(
            Kind::EncryptedDirectMessage,
            msg,
            vec![Tag::custom(
                TagKind::SingleLetter(tag.clone()),
                vec![dst.to_string()],
            )],
        );

        let event = if dst == user {
            cwk.client.sign_event_builder(builder).await?
        } else {
            // signal sign by random key
            let keys = Keys::generate();
            builder.to_event(&keys)?
        };

        let resp = cwk
            .client
            // .send_event_builder(builder)
            .send_event_with(event, move |url, event| event_with_cashu(&(), url, event))
            .await
            .inspect_err(|_e| *code = 500)?;

        let wm = WsMessage::new(200).data(EventOutput::new(resp).json());
        Ok(wm)
    }
}

use std::collections::BTreeMap as Map;
use std::collections::BTreeSet as Set;
#[derive(Clone, Default, Debug, Serialize, Deserialize)]
pub struct EventOutput {
    pub val: String,
    pub success: Set<String>,
    pub failed: Map<String, Option<String>>,
}

impl EventOutput {
    fn new(out: Output<EventId>) -> Self {
        let Output {
            val,
            success,
            failed,
        } = out;

        Self {
            val: val.to_hex(),
            success: success.iter().map(|s| s.to_string()).collect(),
            failed: failed
                .into_iter()
                .map(|(k, v)| (k.to_string(), v))
                .collect(),
        }
    }
    fn json(&self) -> String {
        serde_json::to_string(&self).unwrap()
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
        PublicKey::from_hex(p)?;
        let c = state.clients.get(*p).map(|c| c.clone());
        if c.is_none() {
            let wm = WsMessage::default().code(404);
            metas.push(wm);
            continue;
        };

        let filter = Filter::new()
            .pubkey(p.parse()?)
            // .custom_tag(SingleLetterTag::lowercase(Alphabet::P), vec![*p])
            .kinds([Kind::Custom(0)])
            .limit(1);
        let events = c
            .unwrap()
            .client
            .get_events_of(
                vec![filter],
                EventSource::both(Some(Duration::from_millis(state.config.timeout_ms))),
            )
            .await;
        // let meta = c.unwrap().metadata(p.parse()?).await;
        match events.and_then(|s| {
            s.into_iter()
                .next()
                .ok_or_else(|| nostr_sdk::client::Error::MetadataNotFound)
        }) {
            Ok(m) => {
                debug!("metadata: {:?}", serde_json::to_string(&m).unwrap());

                let mut msg = EventMsg::default();
                msg.id = m.id.to_hex();
                msg.ts = m.created_at.as_u64() * 1000;
                msg.from = m.pubkey.to_hex();
                msg.content.replace(m.content.clone());
                msg.kind = m.kind.as_u16();
                msg.to = m
                    .tags
                    .iter()
                    .filter_map(|ts| {
                        let slice = ts.clone().to_vec();
                        if slice.len() >= 2 && slice[0] == "p" {
                            Some(slice[1].to_string())
                        } else {
                            None
                        }
                    })
                    .next()
                    .unwrap_or_default();

                let js = serde_json::to_string(&msg)?;
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
    if metas.iter().all(|m| m.code == 404) {
        bail!("the keys subcribed all not inited");
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
                    Ok(mut a) => {
                        info!("ws-{who} recv broadcasted events: {}", a.id);
                        if pubkeys.contains(&a.to.as_ref()) {

                            if a.content.is_some() {
                                if let Ok(mut js) = serde_json::from_str::<Value>(a.content.as_ref().unwrap()) {
                                    if let Some(v) = js.get("payToken").cloned() {
                                        if let Some(s) = v.as_str() {
                                            match api_cashu::decode_token(s.trim().to_string()) {
                                                Ok(i) => {
                                                    // let ijs = serde_json::to_string(&i)?;
                                                    js["payTokenDecode"] = serde_json::to_value(i)?;
                                                    let js2 =serde_json::to_string(&js)?;
                                                    a.content.replace(js2);
                                                }
                                                Err(e) => {
                                                    warn!("{} decode_token failed ignore: {} {}", a.id, e, s);
                                                }
                                            }
                                        }
                                    }
                                }
                            }

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
                let msg =  match msg {
                    Message::Text(text) =>  text,
                    Message::Ping(_) =>   continue,
                   other => bail!("unknown message: {:?}", other),
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
