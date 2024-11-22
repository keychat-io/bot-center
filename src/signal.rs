#[derive(Default, Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Handshake {
    pub c: String,
    #[serde(rename = "type")]
    pub typo: i32,
    pub msg: String,
    pub name: String,
}

#[derive(Default, Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HandshakeName {
    pub name: String,
    pub pubkey: String,
    #[serde(rename = "curve25519PkHex")]
    pub curve25519pk_hex: String,
    pub onetimekey: String,
    pub signed_id: i64,
    pub signed_public: String,
    pub signed_signature: String,
    pub prekey_id: i64,
    pub prekey_pubkey: String,
    pub global_sign: String,
    pub relay: String,
    pub time: i64,
}

#[derive(Clone)]
pub struct SignalKeys {
    pub prikey: String,
    pub pubkey: String,
    pub keypair: KeychatIdentityKeyPair,
}

pub fn generate_signal_keypair(nostrsk: &str) -> anyhow::Result<SignalKeys> {
    use api_signal::signal_store::libsignal_protocol::KeyPair;
    use api_signal::signal_store::libsignal_protocol::PrivateKey;
    use bitcoin_hashes::Hash;

    let gen = format!("{}.{}.{}", nostrsk, "signal", "bot-center");
    let hash = bitcoin_hashes::sha512_256::Hash::hash(gen.as_bytes());
    let prik = PrivateKey::deserialize(hash.as_ref())?;
    let kp: KeyPair = prik.try_into()?;

    let sbytes = kp.private_key.serialize();
    let pbytes = kp.public_key.serialize();
    let prikey = hex::encode(&sbytes);
    let pubkey = hex::encode(&pbytes);
    let keypair = KeychatIdentityKeyPair {
        private_key: sbytes.try_into().unwrap(),
        identity_key: pbytes.to_vec().try_into().unwrap(), //+1
    };
    let this = SignalKeys {
        prikey,
        pubkey,
        keypair,
    };

    Ok(this)
}

use keychat_rust_ffi_plugin::api_cashu::cashu_wallet::cashu::util::hex;
use keychat_rust_ffi_plugin::api_nostr;
use keychat_rust_ffi_plugin::api_signal;
use keychat_rust_ffi_plugin::api_signal::KeychatIdentityKey;
use keychat_rust_ffi_plugin::api_signal::KeychatIdentityKeyPair;
use keychat_rust_ffi_plugin::api_signal::KeychatProtocolAddress;
use serde_json::json;

pub async fn init_db(path: String) -> anyhow::Result<()> {
    tokio::task::spawn_blocking(|| api_signal::init_signal_db(path)).await??;
    Ok(())
}

pub async fn init_keypair(keys: &SignalKeys) -> anyhow::Result<()> {
    let keypair = keys.keypair.clone();
    tokio::task::spawn_blocking(move || api_signal::init_keypair(keypair, 0)).await??;
    Ok(())
}

async fn process_prekey_bundle(msg: HandshakeName, keys: &SignalKeys) -> anyhow::Result<()> {
    let keypair = keys.keypair.clone();
    let remote_address = KeychatProtocolAddress {
        // name: msg.pubkey.clone(),
        name: msg.curve25519pk_hex.clone(),
        device_id: 1,
    };
    let identity_key = KeychatIdentityKey {
        public_key: hex::decode(&msg.curve25519pk_hex)?.try_into().unwrap(),
    };

    tokio::task::spawn_blocking(move || {
        api_signal::process_prekey_bundle_api(
            keypair.clone(),
            remote_address,
            1,
            1,
            identity_key,
            msg.signed_id.try_into().unwrap(),
            hex::decode(&msg.signed_public)?,
            hex::decode(&msg.signed_signature)?,
            msg.prekey_id.try_into().unwrap(),
            hex::decode(&msg.prekey_pubkey)?,
        )
    })
    .await??;

    Ok(())
}

use crate::db::*;
use crate::EventMsg;

pub fn try_decode_handshake(wrap: &mut EventMsg) -> anyhow::Result<(Handshake, HandshakeName)> {
    let content = wrap.content.as_deref().unwrap_or_default();
    let msg = serde_json::from_str::<Handshake>(content)?;
    // info!("msg: {:?}", msg);
    if !(msg.c == "signal" && msg.typo == 101) {
        bail!("signal && type 101")
    }
    wrap.comfirmed = true;

    let msg_name = serde_json::from_str::<HandshakeName>(&msg.name)?;
    ensure!(wrap.from == msg_name.pubkey, "unmatched pubkey");

    Ok((msg, msg_name))
}
pub async fn handle_handshake(
    _mypubkey: &str,
    _pubkey: &str,
    _msg: Handshake,
    msg_name: HandshakeName,
    keys: &SignalKeys,
    db: &LitePool,
) -> anyhow::Result<()> {
    process_prekey_bundle(msg_name.clone(), keys).await?;

    // clear old session
    let session = db.get_session(_pubkey, _mypubkey).await?;
    if let Some(_s) = session {
        let keypair = keys.keypair.clone();
        let rm = db.remove_session_and_receivers(_pubkey, _mypubkey).await?;

        let bob_address = KeychatProtocolAddress {
            name: _s.pubkey.to_owned(),
            device_id: 1,
        };

        let res =
            tokio::task::spawn_blocking(move || api_signal::delete_session(keypair, bob_address))
                .await;
        info!(
            "api_signal::delete_session {}->{}: {:?} {:?}",
            _mypubkey, _pubkey, rm, res
        );
    }

    db.insert_session(
        &msg_name.pubkey,
        _mypubkey,
        unix_time_ms(),
        &msg_name.name,
        &msg_name.curve25519pk_hex,
        &msg_name.onetimekey,
    )
    .await?;
    Ok(())
}

/// encrypt msg, my_receiver_addr, msg_keys_hash, alice_addrs_pre
pub async fn encrypt_msg(
    mut msg: String,
    curve25519_pubkey: &str,
    pubkey: &str,
    nostrpk: &str,
    nostrsk: &str,
    keys: &SignalKeys,
    pre: bool,
) -> anyhow::Result<(Vec<u8>, Option<String>, String, Option<Vec<String>>)> {
    let keypair = keys.keypair.clone();

    let bob_address = KeychatProtocolAddress {
        name: curve25519_pubkey.to_owned(),
        device_id: 1,
    };

    // for onetimekey only
    if pre {
        msg = generate_pre_key_response(nostrpk, nostrsk, &msg, curve25519_pubkey, pubkey)?;
    }

    // encrypt msg, my_receiver_addr, msg_keys_hash, alice_addrs_pre
    let res = tokio::task::spawn_blocking(move || {
        api_signal::encrypt_signal(keypair, msg, bob_address, None)
    })
    .await??;
    Ok(res)
}

// packages/app/lib/service/signal_chat_util.dart
fn generate_pre_key_response(
    nostrpk: &str,
    nostrsk: &str,
    message: &str,
    curve25519_pubkey: &str,
    pubkey: &str,
) -> anyhow::Result<String> {
    let ident = nostrpk;

    let mut strs = [&ident, pubkey, &message];
    strs.sort();
    let content = strs.join(",");
    info!("{}", content);

    let sig = api_nostr::sign_schnorr(nostrsk.to_owned(), content)?;

    let js = json!({
        "nostrId": &ident,
        "name": curve25519_pubkey,
        "message": message,
        "sig": sig,
    });

    let str = serde_json::to_string(&js)?;
    Ok(str)
}

/// decrypt msg, userid
pub async fn decrypt_msg(
    content: &str,
    nostrpk: &str,
    receiverkey: &str,
    keys: &SignalKeys,
    db: &LitePool,
) -> anyhow::Result<(String, String)> {
    let ciphertext = base64_simd::STANDARD.decode_to_vec(&content)?;
    let (userid, _ts, pubkey, onetimekey) = db
        .get_receiver(receiverkey)
        .await?
        .ok_or_else(|| format_err!("signal receiver key not found"))?;
    let res = decrypt_msg2(ciphertext, &pubkey, keys).await?;

    if onetimekey.unwrap_or_default().len() >= 1 {
        db.take_onetimekey(&userid, &nostrpk).await?;
    }

    Ok((res.0, userid))
}

/// decrypt msg, msg_keys_hash, alice_addr_pre
pub async fn decrypt_msg2(
    ciphertext: Vec<u8>,
    curve25519_pubkey: &str,
    keys: &SignalKeys,
) -> anyhow::Result<(String, String, Option<Vec<String>>)> {
    let keypair = keys.keypair.clone();

    let bob_address = KeychatProtocolAddress {
        name: curve25519_pubkey.to_owned(),
        device_id: 1,
    };

    let (bytes, keyhash, _pre) = tokio::task::spawn_blocking(move || {
        api_signal::decrypt_signal(keypair, ciphertext, bob_address, 0, !true)
    })
    .await??;

    let str = String::from_utf8(bytes)?;
    Ok((str, keyhash, _pre))
}

use keychat_rust_ffi_plugin::api_signal::KeychatSignalSession;
pub async fn get_session(
    curve25519pk_hex: String,
    keys: &SignalKeys,
) -> anyhow::Result<Option<KeychatSignalSession>> {
    let keypair = keys.keypair.clone();

    let res = tokio::task::spawn_blocking(move || {
        api_signal::get_session(keypair, curve25519pk_hex, 1.to_string())
    })
    .await??;
    Ok(res)
}
