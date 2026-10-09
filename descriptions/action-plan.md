# Discordium action plan

Discordium is a new pNet app. It is not a change to pNet, and it is not a
continuation of `pnet_chat`. `pnet_chat` stays the separate preview it is.

## The setup

One Rust binary, started by a person on each machine that should take part.
The node approves it in Config. pNet stays a dumb pipe: register, get-data,
send, and push. pNet does not know what a conversation is, and it does not
store one.

The process reads its own device from get-data and picks a role:

| Grade | Role |
|---|---|
| Device | The interface. Show conversations, accept what the person types, and ask this user's server for the record. |
| Server | The record. Save every conversation and every message. When a device asks, send that device the part it asked for. |

A device does not keep the record. Restarting it, or moving to another
device, comes back by asking the server again. A server does not serve the
chat interface. Its job is to save and to answer.

A device-grade node does not serve the owner portal, so the interface cannot
be a portal page. Discordium serves its own pages on the device. The server
process speaks the app protocol only.

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
  installs `pnet` only. Someone starts `discordium` on the device and on the
  server.

If this user has more than one server, the lowest `sg_rank` holds the record.
Any other server-grade process starts, says it is standing by, and does not
open a second store.

## How a message moves

1. The device asks the server to open a conversation.
2. The server saves it and answers with that conversation.
3. The device sends a message in that conversation.
4. The server saves the message and answers with the saved copy.
5. The device shows that saved copy.
6. Another of this user's devices, if it is attached, hears that the
   conversation changed and then asks for the new messages.

The server does not push the record on its own. A device receives a
conversation by asking. pNet send does not retry and does not promise
delivery, so Discordium acks and the sender retries. Applying the same
message twice is the same as applying it once.

Payloads stay inside one app datagram. The fabric ceiling is 4096 bytes.
Stay near 1 KiB so a packet is not fragmented on the way. A long history is
several asks, each continuing from the last message the device already has.

## Identity

Register as alias `discordium` and protocol `application/discordium`.
Discovery uses the alias. The `app_id` comes back from get-data and is
different on every device.

The server accepts a request only when the push's `sender_app_id` resolves
to an approved `discordium` on one of this user's own devices. A claim
inside the payload is not the identity. Until get-data shows that app, the
server waits. It does not treat "not visible yet" as a permanent refusal.

The device finds the server the same way: own devices, grade server, lowest
`sg_rank`, approved alias `discordium`, then that device's `app_id`.

## Store

On the server, under `~/.pnet/discordium/`. The app owns this directory.
Nothing goes into `node.toml`.

A conversation has an id, a title, and a created time. A message has an id
chosen by the device, the conversation id, the sending device, a time, and
the text. The server's order is the order. The device message id makes a
retry land once.

## Interface

Loopback HTTP on the device, default `127.0.0.1:8788`.

- A control that opens a conversation. The new row appears after the server answers.
- The list of conversations, fetched from the server.
- One conversation, with its messages fetched from the server.
- A box that sends a message and then shows what the server saved.

Relative links, so the pages work on that port alone. No second password.

## Work, in order

Each step is its own change, branched from `develop`, and lands through a
pull request into `develop`. Steps 1–8 are finished when their tests pass.
Step 9 is finished when the two-node run has been done. Later steps call
the code the earlier steps already merged. They do not reopen it.

Payloads are opaque to pNet. Every one of them begins with a version byte
and a type byte. The server reads or writes only after the push's
`sender_app_id` resolves to an approved `discordium` on one of this user's
own devices.

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

### 9. Two nodes

Write the run commands in the README. `PNET_AUTO_APPROVE_APPS` stays a test
switch. A real node approves the app by hand.

Finished when these have been done on a running server and two devices:

- The device serves the pages. The server process does not.
- A message sent on the first device is still there after that process
  restarts, because the server kept it.
- The second device sees that message by asking.

## Done when

Step 9 has been run, so a person can open the interface on a device, leave,
come back, and see the same conversations on another of their devices. The
check that a stranger changes nothing is step 3. pNet core is untouched.
