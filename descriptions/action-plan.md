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

1. The device UI sends the message to the server's Discordium.
2. The server saves it and answers with the saved message.
3. The device shows that saved copy.
4. Another of this user's devices, if it is attached, hears that the
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

Loopback HTTP on the device, default `127.0.0.1:8788`. Three views:

- The list of conversations, fetched from the server.
- One conversation, with its messages fetched from the server.
- A box that sends a message and then shows what the server saved.

Relative links, so the pages work on that port alone. No second password.

## Work, in order

Each step is its own change, branched from `develop`, and lands through a
pull request into `develop`.

### 1. Crate and role

- Cargo package `discordium`, edition 2021, one binary.
- Register with the local node, remember the token, and call get-data.
- Choose device, record-holding server, or standby from that tree.
- Tests use a scripted get-data reply, not a live node.

### 2. Record

- Create and reopen `~/.pnet/discordium/`.
- Add a conversation, append a message, list conversations, and read
  messages after a cursor.
- A repeated device message id does not create a second row.
- Tests cover an empty store, a reopen after process exit, and the repeat.

### 3. Ask and answer

App payloads, opaque to pNet:

| Type | Direction | Body |
|---|---|---|
| `HELLO` | device → server | The device is up. |
| `HELLO_ACK` | server → device | The server accepts this device. |
| `LIST_REQ` / `LIST_RESP` | device ↔ server | The conversation list. |
| `HISTORY_REQ` / `HISTORY_RESP` | device ↔ server | Messages after a cursor, and whether more remain. |
| `POST` / `POST_ACK` | device ↔ server | Save one message, or return the copy already saved. |
| `NOTICE` | server → attached device | A conversation changed. The device then sends `HISTORY_REQ`. |

The server checks the stamped `app_id` before it reads or writes. A device
retries until `POST_ACK` or `HELLO_ACK`. Tests encode each message and run
both roles in one process with a fake socket.

### 4. Device pages

- Serve the three views on `127.0.0.1:8788`.
- The list and the thread render only what the server returned.
- Sending a message waits for `POST_ACK`, then draws the saved text.
- A `NOTICE` triggers `HISTORY_REQ` for that conversation.

### 5. Run it on two nodes

Document the commands in the README:

- Start pNet on a server and on a device.
- Start `discordium` on both.
- Approve both in Config → Pending Apps.
- Open the device UI, send a message, stop the device process, start it
  again, and see the same message because the server still has it.

`PNET_AUTO_APPROVE_APPS` stays a test switch. A real node approves the app
by hand.

## Done when

- A device shows the interface, and a server of the same user does not.
- The server still has every message after the device process is gone.
- A second device of the same user sees those messages by asking.
- A payload whose sender is not this user's approved Discordium changes
  nothing.
- pNet core is untouched.
