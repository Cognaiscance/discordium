# Improve Discordium: interface, then voice, then webcam

The first version is done. This plan adds three phases, in this order. Each step is its own branch from `develop` and its own pull request into `develop`. A later step starts only after the earlier one is merged. The first pull request after this plan is approved is step 1 only.

Text stays the record. Voice and video are live only. They are not written into `~/.pnet/discordium/record`. pNet core, the installer, and the owner portal stay unchanged. Other pNet users stay out. The server process still does not open a window and still does not touch a microphone or a camera.

Payloads stay one app datagram: version byte, type byte, then the body. The ceiling is 4096 bytes, and the aim is still near 1 KiB. The next free type byte is `0x0D`. The live record is magic `DMR1`. A new server must open that file and keep the conversations already there.

n64 is the record server (glibc 2.41). A server binary for n64 is built in the `rust:1-bookworm` container, not with the dev machine's newer glibc. tealface is the Pi 3 client (about 1 GB RAM, no Rust toolchain). A client binary for tealface is a release build for `aarch64-unknown-linux-gnu`, using the `deploy-tealface` skill. sanosuke is the local x86_64 client.

## 1. Interface

The side list and the message column stay. The window still shows only what the server returned. The record already stores each message's device id and `sent_ms`. The window currently draws only `message.text`.

### Step 1. Show the conversation

- The main panel heading is the open conversation's title.
- Each message shows the sending device's alias, the local time, then the text. The alias comes from the directory the client already loaded. A device with no alias is shown as the first four bytes of its id in hex. The text stays raw (`a<b c` stays those characters).
- A Back control clears the open conversation and returns the main panel to the list prompt. The side list stays.
- Notes stay on screen in both the list and the open conversation.
- Enter still opens and sends, the same as the buttons.

Finished when a test builds the message line from a saved device id and time, and the window code uses that line. No new wire type.

### Step 2. Make the composer match the action

- The title box is the control for a new conversation. Its button stays Open. Enter still opens.
- The message box is the control for the open conversation. Its button stays Send. Enter still sends.
- The empty main panel says to pick a conversation from the list or to open one with the title box.
- An error from the server ("A title is required.", "A message is required.", "The server is not visible yet.", "That title is too long.", "That message is too long.", "The server did not answer.") stays next to the box that caused it.

Finished when those two boxes still call the same open and send paths, and an empty title or an empty message still produces the same note as today.

## 2. Voice channels

A voice channel is a conversation with a kind. Text conversations stay kind text. Existing `DMR1` rows are text. The store learns to read `DMR1` and to write a newer record that adds one kind byte per conversation. Opening the live n64 file still returns the conversations already saved.

A voice channel has the same text thread as a text conversation. People can type in it. Joining it also starts microphone audio to the other joined clients.

### Step 3. Kind on the record, and the list shows it

- Creating a conversation can ask for text or voice. The title box stays for the name. A second control, Voice, opens a voice channel with that title. Open still opens a text conversation.
- `CREATE_REQ` gains a kind. Old clients that omit it create a text conversation. The server answer and `LIST_RESP` include the kind.
- The side list marks a voice channel so it is distinct from a text conversation.
- `ADDED` still tells the other attached clients, and they refresh the list. The kind comes from `LIST_RESP`, not from the notice.

Finished when a test creates one text conversation and one voice channel, reopens a `DMR1` file unchanged, and the list test shows the kind the server saved.

### Step 4. Join, leave, and forward audio

New type bytes, in order, starting at `0x0D`:

- Join. The server remembers this attached client in that voice channel. Membership is in memory on the record process. A restart clears it. It is not a row in the record.
- Leave. A dropped client is treated as a leave.
- Membership. The server sends the current set to the other attached clients of that channel.
- Audio. One Opus frame: conversation id, a sequence number, then the frame. The server forwards it to the other joined clients and not back to the sender. It does not retry a frame. A duplicate or older sequence is dropped by the receiver. The frame is not stored.

The server does not open a sound device. Tests use a fake socket: a joined peer receives the frame, the sender does not, a client who did not join does not, and history does not grow.

The encoder is an Opus crate that the bookworm container and the aarch64 cross container can compile. If it needs a C library, that library is installed in those containers at build time. tealface gets only a runtime library if the binary is dynamically linked, not a compiler.

### Step 5. Talk in the window

- Opening a voice channel shows Join, Leave, and Mute, and the aliases of who is joined.
- Join captures the default microphone and plays remote frames. Leave stops both. Mute stops sending and keeps playback.
- No microphone, or a capture error, shows a note and leaves the text thread usable.
- The server binary stays headless. Clients on sanosuke and tealface are the programs that open the sound device.

Finished when the window controls call the join, leave, and mute paths, and a machine with no microphone still shows the text.

## 3. Optional webcam

The camera is a control on a voice channel. It is off until the person turns it on. Voice keeps working with the camera off, and on a machine that has no camera.

### Step 6. Forward a picture

- A new type carries one picture piece: conversation id, frame number, piece index, piece count, then bytes.
- Each datagram stays at or under 4096 bytes. A picture that does not fit is split. The receiver shows it only when every piece of that frame number has arrived. A missing piece drops that frame.
- The server forwards pieces to the other joined clients of that voice channel, and not to the sender. It does not decode them and does not store them.
- A client who has not joined does not get the pieces.

Finished when a fake-socket test reassembles one split picture and drops a frame that is missing a piece.

### Step 7. Camera control

- A voice channel has Camera on and Camera off. The default is off.
- On starts the platform camera, shows the local preview, and sends frames. Off stops sending and removes the preview.
- Other joined clients' pictures appear above the text while those clients have the camera on.
- No camera, or a refused device, shows a note. Join, audio, and text keep working.
- tealface may have no camera. That is a note, not a failed client.
- Screen share stays out.

Finished when the control sends frames only while it is on, and a missing camera does not stop voice.

## Out of this plan

- A second record server and failover.
- Discordium accounts, and other pNet users.
- Attachments, screen share, and any change to pNet.
- Storing voice or video in the record.
