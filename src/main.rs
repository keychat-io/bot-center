#[macro_use]
extern crate anyhow;
#[macro_use]
extern crate serde;
#[macro_use]
extern crate tracing;

use keychat_rust_ffi_plugin::api_cashu;

mod config;
use config::{Config, Opts};
mod db;
use db::LitePool;
mod routes;
mod signal;
use routes::*;

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

                let _opts = SubscribeAutoCloseOptions::default().filter(opts);
                let res = c
                    .client
                    .subscribe_with_id(SubscriptionId::new("s0"), vec![subscription], None)
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
                            tokio::time::sleep(sleep).await;
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

    use axum::{routing, Router};
    use tower_http::trace::{DefaultMakeSpan, TraceLayer};
    // build our application with some routes
    let app = Router::new()
        .route("/metadata/:key", routing::post(post_metadata))
        .route("/event/from/:from/to/:to", routing::post(post_event))
        .route("/ws", routing::any(ws_handler))
        .route("/balance", routing::get(get_balance))
        .route("/receive", routing::post(post_receive))
        .route("/send", routing::post(post_send))
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
use std::{net::SocketAddr, sync::atomic::AtomicU64, sync::Arc, time::Duration};
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

use dashmap::DashMap;
use signal::SignalKeys;
use tokio::sync::broadcast;
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
