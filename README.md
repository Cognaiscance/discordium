# Discordium

A pNet app for conversations. The same Rust program runs on a device and on a
server, and the grade of that node decides the role.

A device-grade node serves Discordium's own interface. A person reads and
writes conversations there. A server-grade node keeps the conversation record
and gives it to a device when the device asks. pNet only carries the bytes.

The build steps are in [descriptions/action-plan.md](descriptions/action-plan.md).
There is no crate yet.
