# Az.Local.Dev

A local, self-contained emulator for Azure resources — currently **Azure Service Bus** — with a system-tray
app and a browser dashboard for managing everything, so you can develop and test against Azure SDKs without
a real Azure subscription.

Speaks real **AMQP 1.0** on the wire, so unmodified apps using the official `Azure.Messaging.ServiceBus` SDK
(.NET, Java, JS, Python, Go) can connect to it exactly as they would to a real namespace, just by pointing a
connection string at `localhost`.

## Features

- **System tray app** (`emu-engine.exe`) — runs quietly in the background; left-click the tray icon to open
  the dashboard, right-click for a Quit/Open menu.
- **Web dashboard** at [http://127.0.0.1:7777](http://127.0.0.1:7777) — create, rename, start/stop, and
  delete resource groups and resources; browse queues and peek/complete/abandon/dead-letter/resubmit
  messages; global search across resource groups and resources.
- **Azure Service Bus emulation** — queues with active/scheduled/deferred/dead-lettered message states,
  AMQP 1.0 (plain + TLS) endpoints starting at port `5672`, one emulated namespace per resource.
- **Auto-persistence** — every resource group is saved as its own JSON file under
  `%APPDATA%/EmuEngine/groups/{group-id}.json`, kept in sync on every create/rename/delete, and restored
  automatically on the next launch. Queue/message data is stored separately under
  `%APPDATA%/EmuEngine/data/service-bus/{instance-id}.json`.

## Project layout

This is a Cargo workspace:

| Crate | Path | Purpose |
| --- | --- | --- |
| `emu-servicebus-core` | `emu/module/servicebus/core` | Domain model: broker, entities, message states — no I/O. |
| `emu-servicebus-amqp` | `emu/module/servicebus/amqp` | AMQP 1.0 server adapter (`fe2o3-amqp`) over the core broker. |
| `emu-servicebus-engine` | `emu/module/servicebus/engine` | Wires core + AMQP into a runnable Service Bus emulator instance, plus its REST API. |
| `emu-registry` | `emu/services/engine` | Generic `EmulatorEngine`/`EngineRegistry` traits shared by every resource kind. |
| `emu-web` | `emu/ui/web` | Dashboard REST API, static asset serving, and per-group persistence. |
| `emu-gui` | `emu/ui/gui` | The tray application binary (`emu-engine`), wiring everything together. |

## Building & running

```powershell
cargo build -p emu-gui
./target/debug/emu-engine.exe
```

The dashboard opens automatically on first launch; afterwards, use the tray icon to reopen it or quit.

## Status

This is a development/test emulator, not a clone of the full Azure control plane — see [DESIGN.md](DESIGN.md)
for the detailed design, goals, and non-goals.
