# Keychat Chat Bot Center

## Description

* Receive users messages from relays
* Send messages to user by relays
* Manager ecash

## Project setup

### Install Rust

https://www.rust-lang.org/tools/install

### configure .env and bc.toml
```bash
# copy from .env.example
$ cp .env.example .env

# input your bot BOT_CENTER_SECRETS, Hex format(without 0x, lowercase), multiple use ';' separate

# modify bc.toml according to your needs
```

### run the project

```bash
# development
$ cargo run -- -v

# production mode
$ cargo run -r -- -v

# build 
$ cargo build -r 
```

## Apis

### post /metadata/:bot_pubkey update bot's metadata
```bash
curl -X POST -H 'Content-type: application/json' --data '{"name":"ChatGPT","description":"I am a chatbot that can help you with your queries. Pay ecash for each message you send.","pubkey":"npub1p4sae59men07dv3zlp786ujd5vzs26003hr0ymhh6axjf6hajl9s652cfl","commands":[{"name":"/h","description":"Show help message"},{"name":"/m","description":"Pay per message plan"}],"botPricePerMessageRequest":{"type":"botPricePerMessageRequest","message":"Please select a model to chat","priceModels":[{"name":"GPT-4o","description":"","price":0,"unit":"sat","mints":[]},{"name":"GPT-4o-mini","description":"","price":2,"unit":"sat","mints":[]},{"name":"GPT-4-Turbo","description":"","price":3,"unit":"sat","mints":[]}]}}' http://0.0.0.0:5001/metadata/0d61dcd0bbccdfe6b222f87c7d724da3050569ef8dc6f26ef7d74d24eafd99cc

# {"code":200,"error":null,"data":"{\"val\":\"0d61dcd0bbccdfe6b222f87c7d724da3050569ef8dc6f26ef7d74d24eafd99dd\",\"success\":[\"wss://relay.keychat.io/\"],\"failed\":{}}"}
```

### post /event/from/:bot_pubkey/to/:user_pubkey send event to a user
```bash
curl -X POST -H 'Content-type: application/json' --data '{"name":"ChatGPT","description":"I am a chatbot that can help you with your queries. Pay ecash for each message you send.","pubkey":"npub1p4sae59men07dv3zlp786ujd5vzs26003hr0ymhh6axjf6hajl9s652cfl","commands":[{"name":"/h","description":"Show help message"},{"name":"/m","description":"Pay per message plan"}],"botPricePerMessageRequest":{"type":"botPricePerMessageRequest","message":"Please select a model to chat","priceModels":[{"name":"GPT-4o","description":"","price":0,"unit":"sat","mints":[]},{"name":"GPT-4o-mini","description":"","price":2,"unit":"sat","mints":[]},{"name":"GPT-4-Turbo","description":"","price":3,"unit":"sat","mints":[]}]}}' http://0.0.0.0:5001/event/from/0d61dcd0bbccdfe6b222f87c7d724da3050569ef8dc6f26ef7d74d24eafd99cc/to/0d61dcd0bbccdfe6b222f87c7d724da3050569ef8dc6f26ef7d74d24eafd99ff

# {"code":200,"error":null,"data":"{\"val\":\"0d61dcd0bbccdfe6b222f87c7d724da3050569ef8dc6f26ef7d74d24eafd99dd\",\"success\":[\"wss://relay.keychat.io/\"],\"failed\":{}}"}
```

### /ws subcribe events from users
```js
// subcribe by bot's pubkey: Hex format(lowercase, without 0x), multiple use ';' separate
socket.send("0d61dcd0bbccdfe6b222f87c7d724da3050569ef8dc6f26ef7d74d24eafd99cc;");
// first message is metadatas from center is [{"code":200,"error":null,"data":'{"id":"","from":"","to":"","ts":1729000000000,"kind":0,"content":""}'}...]
// 
// messages follow is events, to is the pubkey of bot, from is the pubkey of user
{"id":"","from":"","to":"","ts":1729000000000,"kind":4,"content":""}
// 
// Send the event id to the center to indicate that it has been processed
// Hex format(lowercase, without 0x), multiple use ';' separate
```

### get /balance get cashu balances
```sh
curl -v http://0.0.0.0:5001/balance
# {"code":200,"error":null,"data":"{\"https://8333.space:3338/\":1}"}


### post /receive receive cashu token
```sh
curl -X POST -H 'Content-type: application/json' --data 'cashuBo2Ftd2h0dHBzOi8vODMzMy5zcGFjZTozMzM4YXVjc2F0YXSBomFpSAB1nj-LBrNvYXCBo2FhAWFzeEAxZmE5MjA3OGE4OThjYmZkMTgyNDNlMGE4MjY5MGI0NjM2NDk0M2IwOWFiMzBhNWE5NmQ0NGM1ZGRmM2RiMzFjYWNYIQNvnws16oUHGj81iBV05rA6itfAtRnqfgC3WS4CqZVLZw' http://0.0.0.0:5001/receive -v

# return the amount
# {"code":200,"error":null,"data":"1"}
```
