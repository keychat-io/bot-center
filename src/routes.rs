use axum::{
    extract::{
        ws::{Message, WebSocket, WebSocketUpgrade},
        ConnectInfo, Path, State as AxumState,
    },
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    Json,
};

use futures::{sink::SinkExt, stream::StreamExt};
use keychat_rust_ffi_plugin::{api_cashu, api_nostr};
use nostr_relay_pool::Output;
use nostr_sdk::prelude::*;
use serde_json::Value;
use tokio::sync::broadcast;

use crate::db::unix_time_ms;
use crate::signal;
use crate::EventMsg;
use crate::State;
use std::future::Future;
use std::net::SocketAddr;
use std::time::Duration;

pub async fn get_balance(
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

pub async fn post_receive(
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

pub async fn post_metadata(
    sa: ConnectInfo<SocketAddr>,
    state: AxumState<State>,
    Path(key): Path<String>,
    _header: HeaderMap,
    body: String,
) -> impl IntoResponse {
    post_event(sa, state, Path((key, String::new())), _header, body).await
}

pub async fn post_event(
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

pub async fn ws_handler(
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
