# Discordium

A pNet app for conversations. Two programs share one library.

`discordium-client` is the interface. It registers as `discordium-client` and
opens a desktop window. `discordium-server` registers as `discordium-server`,
keeps the record, and does not open a window.
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

The client prints `client` and then `Window open.` The server prints `record`
when this device holds the record, or `standby` when another of this user's
servers does. A standby does not open a store. The server does not open a
window.

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

When a client opens a conversation that was not already saved, the record
server sends `ADDED` to each other attached client. That client sends
`LIST_REQ` and shows the saved list. The client that opened it does not get
`ADDED`. Opening the same conversation again does not send another.

The window lists only the conversations `LIST_RESP` returned. Opening one
waits for `CREATE_RESP`, then that list shows the new row. One conversation's
heading is its title. It shows only the messages `HISTORY_RESP` returned.
Each message shows the sending device's alias, the local time, and the text.
A device with no alias is shown as the first four bytes of its id in hex.
Sending waits for `POST_ACK`, then shows the saved text. Back clears that
conversation and returns to the list prompt. The side list stays. The empty
main panel says to pick a conversation from the list, or to open one with
the title box. A title error stays next to the title box. A message error
stays next to the message box. A `NOTICE` for
that conversation sends `HISTORY_REQ` and adds the new messages. An `ADDED`
updates the list and leaves the open conversation on screen. Enter in the
title box opens a conversation, the same as Open. Enter in the message box
sends it, the same as Send. The window is not a web page.

## Three nodes

`PNET_AUTO_APPROVE_APPS` stays unset. Each node approves its own app by hand,
under Pending Apps.

n64 holds the record:

```
~/.pnet/bin/discordium-server
```

It prints `record` and does not open a window.

sanosuke and tealface are the clients. tealface's node runs as root, so that
binary is `/root/.pnet/bin/discordium-client`. sanosuke uses:

```
~/.pnet/bin/discordium-client
```

Each client prints `client` and then `Window open.` It prints `Hello acked.`
after `discordium-server` and that client are both approved. On tealface,
start that client with `DISPLAY=:0` and
`XAUTHORITY=/var/run/lightdm/root/:0`.

Open a conversation on one client and send a message. Stop that client and
start it again. The conversation is still listed, because the server kept
the record. The other client shows the same message.
