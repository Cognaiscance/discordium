# Discordium action plan

Discordium is a new pNet app. It is not a change to pNet, and it is not a
continuation of `pnet_chat`. `pnet_chat` stays the separate preview it is.

## The setup

One Rust binary, started by a person on each machine that should take part.
The node approves it in Config. pNet stays a dumb pipe: register, get-data,
send, and push. pNet does not know what a conversation is, and it does not
store one.

One program starts a client process, a server process, or both. Both may
run on the same machine, against that machine's local node.

| Process | App name | Role |
|---|---|---|
| Client | `discordium-client` | The interface. Show conversations, accept what the person types, and ask this user's server for the record. Do not keep the record. |
| Server | `discordium-server` | The record. Save every conversation and every message. When a client asks, send that client the part it asked for. Do not open the interface. |

A client does not keep the record. Restarting it, or moving to another
device, comes back by asking the server again. A server does not serve the
chat interface. Its job is to save and to answer.

A device-grade node does not serve the owner portal, so the interface is
Discordium's own. It is not a browser page and not a portal page. The
client opens it. The server process speaks the app protocol only and does
not open it, including when both processes are on that device.

The first version is this user's own devices and this user's server. A
device talks only to that user's Discordium. Other pNet users are a later
decision, after this store and this interface work.

## What the first version leaves out

- Voice, screen share, and attachments.
- A second copy of the record on another server, and failover when the
  server is off.
- Accounts inside Discordium. The person is the user the local pNet node
  already is.
- Any change to pNet core, the installer, or the owner portal. The installer
  installs `pnet` only. Someone starts the client and the server.

The record holder is an approved `discordium-server` on this user's own
devices. A server-grade node wins over a device-grade node. Among
server-grade nodes, the lowest `sg_rank` wins. A rank of 0 means no rank and
sorts last. An equal rank uses the lower device id. Any other server
process starts, says it is standing by, and does not open a second store.
A client on the same machine as the holder uses that local server.

## How a message moves

1. The client asks the server to open a conversation.
2. The server saves it and answers with that conversation.
3. The client sends a message in that conversation.
4. The server saves the message and answers with the saved copy.
5. The client shows that saved copy.
6. Another of this user's clients, if it is attached, hears that the
   conversation changed and then asks for the new messages.

The server does not push the record on its own. A client receives a
conversation by asking. pNet send does not retry and does not promise
delivery, so Discordium acks and the sender retries. Applying the same
message twice is the same as applying it once.

Payloads stay inside one app datagram. The fabric ceiling is 4096 bytes.
Stay near 1 KiB so a packet is not fragmented on the way. A long history is
several asks, each continuing from the last message the device already has.

## Identity

The client registers as alias `discordium-client`. The server registers as
alias `discordium-server`. Both use protocol `application/discordium`.
Discovery uses the alias. The `app_id` comes back from get-data and is
different for each process.

The server accepts a request only when the push's `sender_app_id` resolves
to an approved `discordium-client` on one of this user's own devices. A claim
inside the payload is not the identity. Until get-data shows that app, the
server waits. It does not treat "not visible yet" as a permanent refusal.

The client finds the server by the record-holder rule above: an approved
`discordium-server` on this user's own devices, then that process's
`app_id`.

## Store

On the server, under `~/.pnet/discordium/`. The app owns this directory.
Nothing goes into `node.toml`.

A conversation has an id, a title, and a created time. A message has an id
chosen by the device, the conversation id, the sending device, a time, and
the text. The server's order is the order. The device message id makes a
retry land once.

## Interface

The device shows the interface. The server process does not open one.

The interface a person keeps is not a web browser. The device process draws
it on the terminal where that process was started. Record and standby do
not draw it.

Until that interface exists, a device serves loopback pages at
`127.0.0.1:8788` so the record can be seen:

- A control that opens a conversation. The new row appears after the server answers.
- The list of conversations, fetched from the server.
- One conversation, with its messages fetched from the server.
- A box that sends a message and then shows what the server saved.

Relative links, so the pages work on that port alone. No second password.
Step 11 replaces these pages. The server process does not listen for them.

## Work, in order

Each step is its own change, branched from `develop`, and lands through a
pull request into `develop`. Steps 1–8 are finished when their tests pass.
Step 9 is finished when a client and a server can run on one machine under
those two app names. Step 10 is finished when the two-node run has been
done. Step 11 is finished when a client shows the interface without a
browser and a server does not open one. Later steps call the code the
earlier steps already merged. They do not reopen it. Step 9 is the change
to the single alias and the grade-only role from step 1.

Payloads are opaque to pNet. Every one of them begins with a version byte
and a type byte. The server reads or writes only after the push's
`sender_app_id` resolves to an approved `discordium-client` on one of this
user's own devices.

### 1. Crate and role

Cargo package `discordium`, edition 2021, one binary. Register with the
local node, remember the token, and call get-data. From that tree choose
device, record-holding server, or standby.

Finished when a scripted get-data reply selects each of those three roles.
No live node.

### 2. Record

Create and reopen `~/.pnet/discordium/`. Add a conversation, append a
message, list conversations, and read messages after a cursor. A repeated
device message id does not create a second row.

Finished when tests cover an empty store, a reopen after the process exits,
and that repeated id.

### 3. Hello

`HELLO` from the device, `HELLO_ACK` from the server. The device retries
until the ack. A retry does not attach the device twice.

Finished when both roles run in one process on a fake socket: an approved
own device is acked, and any other sender is ignored.

### 4. Create and list

`CREATE_REQ` / `CREATE_RESP` open a conversation. `LIST_REQ` / `LIST_RESP`
return the conversations. The same client id returns the conversation
already saved.

Finished when a created conversation appears in the list once. Still the
fake socket from step 3.

### 5. Post and history

`POST` / `POST_ACK` save one message, or return the copy already saved.
`HISTORY_REQ` / `HISTORY_RESP` return messages after a cursor and say
whether more remain. The device retries a post until the ack.

Finished when one saved message is read back after a cursor, a repeated
post is one row, and a history that does not fit in one datagram reports
that more remain.

### 6. Notice

`NOTICE` tells each other attached device that a conversation changed. That
device then sends `HISTORY_REQ`.

Finished when device A posts and device B, already attached on the fake
socket, asks and receives the new message.

### 7. Conversation list

Serve `127.0.0.1:8788` on a device. One page lists conversations and opens
a new one. The list renders only what `LIST_RESP` returned. Opening one
waits for `CREATE_RESP`, then shows the new row.

Finished when a page test creates a conversation and the following list
contains that row and no other.

### 8. Thread

A second page shows one conversation. It renders only what `HISTORY_RESP`
returned. Sending waits for `POST_ACK`, then draws the saved text. A
`NOTICE` for this conversation sends `HISTORY_REQ` and adds the new
messages.

Finished when a page test shows an empty thread, a send draws the saved
text, and a notice from a second attached device adds that device's message.

### 9. Client and server

Steps 1–8 registered one alias, `discordium`, and chose the role from the
node grade. This step changes that.

The client process registers as `discordium-client`. The server process
registers as `discordium-server`. Each keeps its own token file, so the two
processes do not replace each other's token. The record file stays with the
server. Each binds its own push port. The defaults are `8790` for the client
and `8791` for the server, so both can bind on one machine. The client
listens on `127.0.0.1:8788`. The server does not.

The client finds an approved `discordium-server` by the record-holder rule.
The server accepts an approved `discordium-client`. A client and a server
on the same machine use that machine's local node for both registrations.

Finished when tests start both processes against one node. The client is
`discordium-client` and serves the pages. The server is `discordium-server`,
serves no pages, and answers the client.

### 10. Two nodes

Write the run commands in the README. `PNET_AUTO_APPROVE_APPS` stays a test
switch. A real node approves each app by hand.

Finished when these have been done on a running server and two clients:

- The client serves the pages. The server process does not.
- A message sent on the first client is still there after that process
  restarts, because the server kept it.
- The second client sees that message by asking.

### 11. Device interface

Replace the loopback pages with the terminal interface. The client process
draws it where it was started. A person does not open a web browser. The
server process does not draw it, and it does not listen on `8788`.

The list shows only what `LIST_RESP` returned. Opening a conversation waits
for `CREATE_RESP`. A thread shows only what `HISTORY_RESP` returned. Sending
waits for `POST_ACK`, then shows the saved text. A `NOTICE` for the open
conversation sends `HISTORY_REQ` and adds the new messages.

Finished when a client run shows that interface without a browser, and a
server run does not open an interface.

## Done when

Step 10 has been run, so a person can use the pages on a client, leave, come
back, and see the same conversations on another of their clients. The client
and the server can also be two processes on one machine. Step 11 replaces
those pages with the terminal interface. The check that a stranger changes
nothing is step 3. pNet core is untouched.
