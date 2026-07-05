use std::sync::Arc;
use std::time::{Duration, Instant};

use tao::event_loop::{ControlFlow, EventLoop};
use tray_icon::menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem};
use tray_icon::{TrayIconBuilder, TrayIconEvent};

use emu_registry::{EmulatorEngine, EngineRegistry};
use emu_servicebus_engine::{ServiceBusEngine, ServiceBusRegistry};
use emu_web::AppState;

mod icon;

const APP_NAME: &str = "Emu Engine";
const DASHBOARD_ADDR: &str = "127.0.0.1:7777";
const SERVICE_BUS_AMQP_PORT: u16 = 5672;

fn tray_icon() -> tray_icon::Icon {
    let (rgba, size) = icon::render_rgba(32);
    tray_icon::Icon::from_rgba(rgba, size, size).expect("valid icon buffer")
}

fn dashboard_url() -> String {
    format!("http://{DASHBOARD_ADDR}/")
}

/// Opens the dashboard in the user's default browser. Since it's just a normal web page,
/// nothing stops the user from opening it in as many tabs/windows as they like.
fn open_dashboard() {
    if let Err(err) = open::that(dashboard_url()) {
        tracing::warn!(?err, "failed to open the dashboard in a browser");
    }
}

fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;

    let registry = EngineRegistry::new();
    let sb_registry = ServiceBusRegistry::new();

    // Shared with the "service-bus" kind factory below so both it (when creating a brand
    // new instance) and the post-restore bump logic further down (when instances were
    // recreated from a persisted resource group instead) agree on the next free port/seq.
    let next_port = Arc::new(std::sync::atomic::AtomicU16::new(SERVICE_BUS_AMQP_PORT + 1));
    let next_seq = Arc::new(std::sync::atomic::AtomicU8::new(2));

    registry.register_kind("service-bus", "Service Bus", {
        // Each instance gets its own `127.0.0.{seq}` loopback address for its AMQPS listener
        // (see `ServiceBusEngine::new`) - that's what lets a Managed-Identity-style
        // `fullyQualifiedNamespace` address a *specific* instance with zero code changes and
        // no custom port, since real Azure SDK clients always dial the default AMQPS port
        // (5671) for a bare hostname. Every 127.0.0.x address is loopback, so no hosts file or
        // DNS setup is needed.
        let next_port = next_port.clone();
        let next_seq = next_seq.clone();
        let sb_registry = sb_registry.clone();
        move |id, name, config| {
            let port = config
                .as_ref()
                .and_then(|c| c.get("port"))
                .and_then(|p| p.as_u64())
                .map(|p| p as u16)
                .unwrap_or_else(|| next_port.fetch_add(1, std::sync::atomic::Ordering::SeqCst));
            let seq = config
                .as_ref()
                .and_then(|c| c.get("seq"))
                .and_then(|p| p.as_u64())
                .map(|p| p as u8)
                .unwrap_or_else(|| next_seq.fetch_add(1, std::sync::atomic::Ordering::SeqCst));
            let engine = Arc::new(ServiceBusEngine::new(id, name, port, seq));
            sb_registry.insert(engine.clone());
            engine as Arc<dyn EmulatorEngine>
        }
    });

    // Every resource group is persisted as its own `%APPDATA%/EmuEngine/groups/{id}.json`
    // file (see `emu_web::persist_group`/`load_all_groups`), rewritten on every rename/
    // create/delete - so this restores exactly what was there last time, names and all,
    // instead of always starting over with a single hardcoded "Service Bus" instance.
    let restored = runtime.block_on(emu_web::load_all_groups(&registry));
    if restored {
        // Bump past whatever ports/instance-seqs were just restored, so a resource created
        // afterward in this run can't collide with one of them.
        for engine in sb_registry.all() {
            next_port.fetch_max(engine.amqp_port() + 1, std::sync::atomic::Ordering::SeqCst);
            let seq = engine.amqps_host().octets()[3];
            next_seq.fetch_max(seq.saturating_add(1), std::sync::atomic::Ordering::SeqCst);
        }
    } else {
        // First-ever run (nothing persisted yet): create the same default this app has
        // always started with, and persist it immediately so it shows up on disk right away
        // rather than only after the user's first edit.
        let service_bus = Arc::new(ServiceBusEngine::new(
            "service-bus-1",
            "Service Bus",
            SERVICE_BUS_AMQP_PORT,
            1,
        ));
        let default_group = registry.create_group("Default", None);
        registry.register(service_bus.clone(), &default_group.id);
        sb_registry.insert(service_bus.clone());
        runtime.block_on(async {
            if let Err(err) = service_bus.start().await {
                tracing::error!(?err, "failed to autostart Service Bus emulator");
            }
        });
        emu_web::persist_group(&registry, &default_group.id);
    }

    let state = AppState {
        registry: registry.clone(),
    };

    let addr = DASHBOARD_ADDR.parse()?;
    let router = emu_web::with_static_fallback(
        emu_web::dashboard_router(state)
            .nest("/api/service-bus", emu_servicebus_engine::router(sb_registry)),
    );
    runtime.spawn(async move {
        if let Err(err) = emu_web::serve(addr, router).await {
            tracing::error!(?err, "dashboard web server crashed");
        }
    });

    // A Win32 message loop needs to be running for the tray icon to work - `tao`'s event
    // loop provides that, even though we never create a visible window.
    let event_loop = EventLoop::new();

    let tray_menu = Menu::new();
    let open_item = MenuItem::new("Open Dashboard", true, None);
    let quit_item = MenuItem::new("Quit", true, None);
    tray_menu.append(&open_item)?;
    tray_menu.append(&PredefinedMenuItem::separator())?;
    tray_menu.append(&quit_item)?;

    let _tray_icon = TrayIconBuilder::new()
        .with_menu(Box::new(tray_menu))
        .with_tooltip(APP_NAME)
        .with_icon(tray_icon())
        .build()?;

    // Open the dashboard once on startup so the user immediately sees something.
    open_dashboard();

    let quit_id = quit_item.id().clone();
    let open_id = open_item.id().clone();

    event_loop.run(move |_event, _target, control_flow| {
        *control_flow = ControlFlow::WaitUntil(Instant::now() + Duration::from_millis(150));

        if let Ok(menu_event) = MenuEvent::receiver().try_recv() {
            if menu_event.id == quit_id {
                runtime.block_on(registry.stop_all());
                *control_flow = ControlFlow::Exit;
            } else if menu_event.id == open_id {
                open_dashboard();
            }
        }

        if let Ok(TrayIconEvent::Click { button, button_state, .. }) = TrayIconEvent::receiver().try_recv() {
            // Only react to a left-click release; a right-click should just show the native
            // context menu (Windows already does this automatically) and not also open the
            // dashboard behind it.
            if button == tray_icon::MouseButton::Left && button_state == tray_icon::MouseButtonState::Up {
                open_dashboard();
            }
        }
    });
}
