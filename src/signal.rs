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

#[derive(Default, Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HandshakeResponsePrekeyMessage {
    #[serde(rename = "nostrId")]
    pub nostrid: String,
    pub name: String,
    pub sig: String,
    pub message: String,
}

use keychat_rust_ffi_plugin::api_cashu::cashu_wallet::cashu::util::hex;
use keychat_rust_ffi_plugin::api_nostr;
use keychat_rust_ffi_plugin::api_signal;
use keychat_rust_ffi_plugin::api_signal::KeychatIdentityKey;
use keychat_rust_ffi_plugin::api_signal::KeychatIdentityKeyPair;
use keychat_rust_ffi_plugin::api_signal::KeychatProtocolAddress;
use serde_json::json;

use std::sync::OnceLock;
static KP: OnceLock<KeychatIdentityKeyPair> = OnceLock::new();
pub fn init(path: String) -> anyhow::Result<()> {
    api_signal::init_signal_db(path)?;

    // a80a565695a21762eb7096437fda80534c00addcab9b09df765ec4b638460050: 05e1fd3e1056a3141a2fa321e859fb19841299234b7e45de1fc944a94feb9b5774
    let prik = "a80a565695a21762eb7096437fda80534c00addcab9b09df765ec4b638460050";
    let pubk = "05e1fd3e1056a3141a2fa321e859fb19841299234b7e45de1fc944a94feb9b5774";
    let keypair = KeychatIdentityKeyPair {
        private_key: hex::decode(prik)?.try_into().unwrap(),
        identity_key: hex::decode(pubk)?.try_into().unwrap(),
    };

    KP.set(keypair.clone()).ok();
    api_signal::init_keypair(keypair, 0)?;

    Ok(())
}

async fn process_prekey_bundle(msg: HandshakeName) -> anyhow::Result<()> {
    let keypair = KP.get().unwrap();
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

pub async fn handle_handshake(_mypubkey: &str, content: &str, _pubkey: &str) -> anyhow::Result<()> {
    let msg = serde_json::from_str::<Handshake>(content)?;
    // info!("msg: {:?}", msg);
    if !(msg.c == "signal" && msg.typo == 101) {
        bail!("signal && type 101")
    }
    let msg = serde_json::from_str::<HandshakeName>(&msg.name)?;
    process_prekey_bundle(msg.clone()).await?;
    HANDSHAKE.lock().await.replace(msg);
    Ok(())
}

use tokio::sync::Mutex;
pub static HANDSHAKE: Mutex<Option<HandshakeName>> = Mutex::const_new(None);
/// encrypt msg, my_receiver_addr, msg_keys_hash, alice_addrs_pre
pub async fn encrypt_msg(
    msg: String,
    curve25519_pubkey: &str,
    pubkey: &str,
    nostrpk: &str,
    nostrsk: &str,
) -> anyhow::Result<(Vec<u8>, Option<String>, String, Option<Vec<String>>)> {
    let keypair = KP.get().unwrap();

    let bob_address = KeychatProtocolAddress {
        name: curve25519_pubkey.to_owned(),
        device_id: 1,
    };

    // for onetimekey only
    let msg = generate_pre_key_response(nostrpk, nostrsk, &msg, curve25519_pubkey, pubkey)?;

    // encrypt msg, my_receiver_addr, msg_keys_hash, alice_addrs_pre
    let res = tokio::task::spawn_blocking(move || {
        api_signal::encrypt_signal(keypair.clone(), msg, bob_address, None)
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

/// decrypt msg, msg_keys_hash, alice_addr_pre
pub async fn decrypt_msg(
    ciphertext: Vec<u8>,
    curve25519_pubkey: &str,
) -> anyhow::Result<(String, String, Option<Vec<String>>)> {
    let keypair = KP.get().unwrap();

    let bob_address = KeychatProtocolAddress {
        name: curve25519_pubkey.to_owned(),
        device_id: 1,
    };

    let (bytes, keyhash, _pre) = tokio::task::spawn_blocking(move || {
        api_signal::decrypt_signal(keypair.clone(), ciphertext, bob_address, 0, !true)
    })
    .await??;

    let str = String::from_utf8(bytes)?;
    Ok((str, keyhash, _pre))
}

use keychat_rust_ffi_plugin::api_signal::KeychatSignalSession;
pub async fn get_session(
    // key_pair: KeychatIdentityKeyPair,
    curve25519pk_hex: String,
    // device_id: String,
) -> anyhow::Result<Option<KeychatSignalSession>> {
    let keypair = KP.get().unwrap().clone();
    info!("spawn_blocking.get_session0.");
    // let address = HANDSHAKE
    //     .lock()
    //     .await
    //     .as_ref()
    //     .unwrap()
    //     .curve25519pk_hex
    //     .clone();
    let res = tokio::task::spawn_blocking(move || {
        info!("spawn_blocking.get_session0");
        api_signal::get_session(keypair, curve25519pk_hex, 1.to_string())
    })
    .await??;
    Ok(res)
}
