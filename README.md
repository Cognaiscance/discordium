# Discordium

A pNet app for conversations. The same Rust program runs on a device and on a
server, and the grade of that node decides the role.

A device-grade node serves Discordium's own interface. A person reads and
writes conversations there. A server-grade node keeps the conversation record
and gives it to a device when the device asks. pNet only carries the bytes.

The build steps are in [descriptions/action-plan.md](descriptions/action-plan.md).

## Role

`cargo run` registers with the local node as alias `discordium` and protocol
`application/discordium`, writes the token to `~/.pnet/discordium/token`, and
reads get-data. `PNET_ADDR` defaults to `127.0.0.1:7777`. `DISCORDIUM_PUSH_PORT`
defaults to `8790`. `DISCORDIUM_TOKEN_FILE` overrides the token path.

A device-grade node prints `device`. The server-grade node with the lowest
`sg_rank` prints `record`. Any other server-grade node prints `standby` and
does not open a store. A rank of 0 means no rank and sorts after every
numbered rank. When two servers still tie, the lower device id holds the
record.

The record role creates or reopens `~/.pnet/discordium/` and keeps every
conversation in the `record` file there. `DISCORDIUM_DIR` overrides that
directory. A device or a standby does not open it. The pages are a later
step. This process stays running so the registration stays bound.
