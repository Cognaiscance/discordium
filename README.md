# Discordium

A pNet app for conversations. Two programs share one library.

`discordium-client` is the interface. It registers as `discordium-client` and
serves loopback pages at `http://127.0.0.1:8788/`. `discordium-server`
registers as `discordium-server`, keeps the record, and does not serve pages.
Both use protocol `application/discordium`. They can run on the same machine,
against that machine's local node.

The build steps are in [descriptions/action-plan.md](descriptions/action-plan.md).

## Run

`cargo run --bin discordium-client` and `cargo run --bin discordium-server`.
`PNET_ADDR` defaults to `127.0.0.1:7777`.

The client push port defaults to `8790`. The server push port defaults to
`8791`. `DISCORDIUM_PUSH_PORT` overrides either one. Each process keeps its
own token: `~/.pnet/discordium-client/token` and
`~/.pnet/discordium-server/token`. `DISCORDIUM_TOKEN_FILE` overrides that path.

The client prints `client` and serves the pages. `DISCORDIUM_HTTP_PORT`
overrides `8788`. The server prints `record` when this device holds the
record, or `standby` when another of this user's servers does. A standby does
not open a store.

The record holder runs `discordium-server` on one of this user's own devices.
A server-grade node wins over a device-grade node. Among server-grade nodes,
the lowest `sg_rank` wins. A rank of 0 means no rank and sorts after every
numbered rank. When two servers still tie, the lower device id holds the
record. The client addresses that server once the app is approved.

The record role creates or reopens `~/.pnet/discordium/` and keeps every
conversation in the `record` file there. `DISCORDIUM_DIR` overrides that
directory. The client does not open it.

A client sends `HELLO` to that server until `HELLO_ACK`. The record server
acks an approved `discordium-client` on one of this user's own devices. A
retry does not attach the client twice. Any other sender is ignored. An app
that is not in get-data yet is not refused; a later hello can still attach. A
standby does not answer.

An approved client opens a conversation with `CREATE_REQ`. The same client id
returns the conversation already saved. `LIST_RESP` returns those
conversations. When they do not fit in one datagram, the reply says more
remain and the next ask continues after the last id.

An approved client saves a message with `POST` and retries until `POST_ACK`.
The same device message id returns the copy already saved. `HISTORY_RESP`
returns messages after a cursor. When they do not fit in one datagram, the
reply says more remain.

When that save is a new message, the record server sends `NOTICE` to each
other attached client. That client sends `HISTORY_REQ` and reads the new
message. A repeated post does not send another notice.

A client serves the conversation list at `http://127.0.0.1:8788/`. The list
shows only the conversations `LIST_RESP` returned. Opening one waits for
`CREATE_RESP`, then that list shows the new row. The server does not listen
for these pages.

The thread page shows one conversation. It renders only the messages
`HISTORY_RESP` returned. Sending waits for `POST_ACK`, then draws the saved
text. A `NOTICE` for that conversation sends `HISTORY_REQ` and adds the new
messages.
