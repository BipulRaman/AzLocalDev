use std::sync::Arc;
use std::time::{Duration, Instant};

use tao::event_loop::{ControlFlow, EventLoop};
use tray_icon::menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem};
use tray_icon::{TrayIconBuilder, TrayIconEvent};

use sbemu_engine::{EmulatorEngine, EngineRegistry};
use sbemu_servicebus_engine::{ServiceBusEngine, ServiceBusRegistry};
use sbemu_web::AppState;

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

    let service_bus = Arc::new(ServiceBusEngine::new(
        "service-bus-1",
        "Service Bus",
        SERVICE_BUS_AMQP_PORT,
        1,
    ));
    let registry = EngineRegistry::new();
    let default_group = registry.create_group("Default", None);
    registry.register(service_bus.clone(), &default_group.id);

    let sb_registry = ServiceBusRegistry::new();
    sb_registry.insert(service_bus.clone());

    registry.register_kind("service-bus", "Service Bus", {
        let next_port = std::sync::atomic::AtomicU16::new(SERVICE_BUS_AMQP_PORT + 1);
        // Instance 1 is the hardcoded default above; dynamically created instances start at 2.
        // Each instance gets its own `127.0.0.{seq}` loopback address for its AMQPS listener
        // (see `ServiceBusEngine::new`) - that's what lets a Managed-Identity-style
        // `fullyQualifiedNamespace` address a *specific* instance with zero code changes and
        // no custom port, since real Azure SDK clients always dial the default AMQPS port
        // (5671) for a bare hostname. Every 127.0.0.x address is loopback, so no hosts file or
        // DNS setup is needed.
        let next_seq = std::sync::atomic::AtomicU8::new(2);
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

    let state = AppState { registry };

    // Autostart the default Service Bus instance so the dashboard has something to show
    // immediately.
    runtime.block_on(async {
        if let Err(err) = service_bus.start().await {
            tracing::error!(?err, "failed to autostart Service Bus emulator");
        }
    });

    let addr = DASHBOARD_ADDR.parse()?;
    let router = sbemu_web::with_static_fallback(
        sbemu_web::dashboard_router(state)
            .nest("/api/service-bus", sbemu_servicebus_engine::router(sb_registry)),
    );
    runtime.spawn(async move {
        if let Err(err) = sbemu_web::serve(addr, router).await {
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
                runtime.block_on(async {
                    service_bus.stop().await.ok();
                });
                *control_flow = ControlFlow::Exit;
            } else if menu_event.id == open_id {
                open_dashboard();
            }
        }

        if let Ok(TrayIconEvent::Click { .. }) = TrayIconEvent::receiver().try_recv() {
            open_dashboard();
        }
    });
}
