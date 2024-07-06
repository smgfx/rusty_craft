//! Windowing code for the engine.

pub mod resources;

pub mod components;
pub mod events;
#[doc(hidden)]
mod icon;
mod systems;

use crate::client::core::window::components::{
    CachedWindow, PrimaryWindow, RawHandleWrapper, Window,
};
use crate::client::core::window::events::{
    CloseRequestedEvent, WindowCreatedEvent, WindowResizedEvent,
};
use crate::client::core::window::resources::WinitWindows;
use crate::client::core::window::systems::{
    l_react_to_resize, l_update_windows, pu_close_windows, pu_exit_on_all_closed,
    pu_exit_on_primary_closed, u_despawn_windows, u_primary_window_check,
};
use bevy_app::prelude::*;
use bevy_app::{AppExit, PluginsState};
use bevy_ecs::event::ManualEventReader;
use bevy_ecs::prelude::*;
use bevy_ecs::system::SystemState;
use raw_window_handle::{HasDisplayHandle, HasWindowHandle};
use tracing::{error, info, warn};
use winit::application::ApplicationHandler;
use winit::event::{StartCause, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::window::WindowId;

/// The plugin which adds a window and associated systems to the app.
///
/// It also overrides the default bevy runner with an event loop.
/// See following fields for how to exit the loop.
pub struct WindowPlugin {
    /// The primary window to create at the start of the program
    /// Can be [`None`] if no primary window is desired
    /// Set primary window parameters here
    pub primary_window: Option<Window>,
    /// The condition at which the event loop will exit.
    ///
    /// See [`ExitCondition`] for more information.
    pub exit_condition: ExitCondition,
}

impl Default for WindowPlugin {
    fn default() -> Self {
        WindowPlugin {
            primary_window: Some(Window::default()),
            exit_condition: ExitCondition::default(),
        }
    }
}

impl Plugin for WindowPlugin {
    fn build(&self, app: &mut App) {
        // Register events
        app.add_event::<CloseRequestedEvent>();
        app.add_event::<WindowCreatedEvent>();
        app.add_event::<WindowResizedEvent>();

        // If a primary window is specified, spawn the entity with the window
        if let Some(primary_window) = &self.primary_window {
            app.world_mut()
                .spawn(primary_window.clone())
                .insert(PrimaryWindow);
        }

        // Add systems to exit the event loop when the condition is met
        match self.exit_condition {
            ExitCondition::OnPrimaryClosed => {
                app.add_systems(PostUpdate, pu_exit_on_primary_closed);
            }
            ExitCondition::OnAllClosed => {
                app.add_systems(PostUpdate, pu_exit_on_all_closed);
            }
            ExitCondition::DontExit => {}
        }

        // Insert resources
        let event_loop = EventLoop::new().unwrap_or_else(|err| {
            panic!("Failed to create event loop with error: {err}");
        });
        app.insert_non_send_resource(event_loop);
        app.insert_non_send_resource(WinitWindows::default());

        // Add systems
        app.add_systems(Update, u_primary_window_check);
        app.add_systems(Update, u_despawn_windows);
        app.add_systems(PostUpdate, pu_close_windows);
        app.add_systems(
            Last,
            (l_update_windows, l_react_to_resize.before(l_update_windows)),
        );

        // Set event loop runner
        app.set_runner(runner);
    }
}

/// This structure contains everything needed in the event loop. It is passed to [`EventLoop::run_app`].
///
/// See [`ApplicationHandler`] and [`EventLoop::run_app`] for more information.
struct WinitApp {
    /// System state used to acquire required objects from the world to create windows
    create_windows_system_state: SystemState<(
        Commands<'static, 'static>,
        Query<'static, 'static, (Entity, &'static mut Window), Added<Window>>,
        NonSendMut<'static, WinitWindows>,
        EventWriter<'static, WindowCreatedEvent>,
    )>,
    /// System state used to acquire required objects to send window events
    window_event_system_state: SystemState<(
        EventWriter<'static, WindowResizedEvent>,
        Query<'static, 'static, (Entity, &'static mut Window)>,
        NonSendMut<'static, WinitWindows>,
    )>,
    /// Bevy App
    app: App,
    /// For reading [`AppExit`] events
    app_exit_event_reader: ManualEventReader<AppExit>,
    /// The [`AppExit`] event that caused the event loop to exit
    app_exit: Option<AppExit>,
}

impl ApplicationHandler for WinitApp {
    fn new_events(&mut self, event_loop: &ActiveEventLoop, cause: StartCause) {
        // Do bevy plugin thing again
        if self.app.plugins_state() == PluginsState::Ready {
            self.app.finish();
            self.app.cleanup();
        }

        // Close the event loop if there is any app exit events
        if let Some(app_exit_events) = self.app.world().get_resource::<Events<AppExit>>() {
            if let Some(app_exit) = self
                .app_exit_event_reader
                .read(app_exit_events)
                .last()
            {
                self.app_exit = Some(app_exit.clone());
                event_loop.exit();
                return;
            }
        }

        // Create any new windows that were added
        let (commands, query, winit_windows, window_created_event) = self
            .create_windows_system_state
            .get_mut(self.app.world_mut());
        create_windows(
            commands,
            query,
            winit_windows,
            window_created_event,
            event_loop,
        );
        self.create_windows_system_state.apply(self.app.world_mut());

        if cause != StartCause::Init {
            return;
        }
        // Create any new windows
        let (commands, query, winit_windows, window_created_event) = self
            .create_windows_system_state
            .get_mut(self.app.world_mut());
        create_windows(
            commands,
            query,
            winit_windows,
            window_created_event,
            event_loop,
        );
        self.create_windows_system_state.apply(self.app.world_mut());
    }

    fn resumed(&mut self, _event_loop: &ActiveEventLoop) {
        // TODO: Actually handle the resumed event for android
    }

    fn window_event(
        &mut self,
        _event_loop: &ActiveEventLoop,
        window_id: WindowId,
        event: WindowEvent,
    ) {
        let (mut window_resized_event, mut query, winit_windows) =
            self.window_event_system_state.get_mut(self.app.world_mut());
        let Some(window_entity) = winit_windows.get_window_entity(window_id) else {
            warn!("Skipped event {event:?} for unknown winit window {window_id:?}");
            return;
        };
        let Ok((_, mut window)) = query.get_mut(window_entity) else {
            warn!("Window {window_entity:?} is missing Window component, skipping event {event:?}");
            return;
        };

        match event {
            WindowEvent::CloseRequested => {
                // Send a close requested event so systems can drop the Window and despawn windows
                self.app.world_mut().send_event(CloseRequestedEvent {
                    entity: window_entity,
                });
            }
            WindowEvent::Resized(size) => {
                window_resized_event.send(WindowResizedEvent {
                    entity: window_entity,
                    new_width: size.to_logical(window.resolution.scale_factor()).width,
                    new_height: size.to_logical(window.resolution.scale_factor()).height,
                });
            }
            WindowEvent::ScaleFactorChanged { scale_factor, .. } => {
                window.resolution.set_scale_factor(scale_factor);
                //info!("Scale factor changed {}, {}, {}", window.resolution.physical_width(), window.resolution.physical_height(), window.resolution.scale_factor());
            }
            _ => {}
        }
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        // Don't update if plugins are not ready
        if self.app.plugins_state() == PluginsState::Cleaned {
            // Run the frame
            self.app.update();

            // Close event loop if received events
            if let Some(app_exit_events) = self.app.world().get_resource::<Events<AppExit>>() {
                if let Some(app_exit) = self
                    .app_exit_event_reader
                    .read(app_exit_events)
                    .last()
                {
                    self.app_exit = Some(app_exit.clone());
                    event_loop.exit();
                }
            }
        }
    }
}

/// The custom runner for the app which runs on the winit event loop.
///
/// Handles window creation, window events and the main client loop.
fn runner(mut app: App) -> AppExit {
    // Bevy stuff that I don't understand
    // Apparently if plugin loading is ready, we need to call finish and cleanup
    if app.plugins_state() == PluginsState::Ready {
        app.finish();
        app.cleanup();
    }

    // Get the event loop from resources
    let event_loop = app
        .world_mut()
        .remove_non_send_resource::<EventLoop<()>>()
        .expect("Event loop should be added before runner is called");

    // System state of added window component
    // We will use this in the event loop to create any new windows that were added
    let create_windows_system_state: SystemState<(
        Commands,
        Query<(Entity, &mut Window), Added<Window>>,
        NonSendMut<WinitWindows>,
        EventWriter<WindowCreatedEvent>,
    )> = SystemState::from_world(app.world_mut());

    let window_event_system_state: SystemState<(
        EventWriter<WindowResizedEvent>,
        Query<(Entity, &mut Window)>,
        NonSendMut<WinitWindows>,
    )> = SystemState::from_world(app.world_mut());

    // Event reader to read any app exit events
    let app_exit_event_reader = ManualEventReader::<AppExit>::default();

    let mut winit_app = WinitApp {
        create_windows_system_state,
        window_event_system_state,
        app,
        app_exit_event_reader,
        app_exit: None,
    };

    // This ensures that new events will be started whenever possible
    // TODO: Maybe change this so that the control flow changes based on other factors like battery saver
    event_loop.set_control_flow(ControlFlow::Poll);

    // Run event loop
    info!("Entered event loop");
    if let Err(err) = event_loop.run_app(&mut winit_app) {
        error!("winit event loop error: {err}");
        return AppExit::error();
    }

    if let Some(app_exit) = winit_app.app_exit {
        app_exit
    } else {
        warn!("Event loop exited without an AppExit event!");
        AppExit::Success
    }
}

/// Creates windows for entities with the [`Window`] component added.
///
/// Helper function called from the runner to create windows with the [`WinitWindows`] resource.
///
/// # Arguments
/// - `commands` - Bevy commands
/// - `query` - Query for entities with the [`Window`] component added
/// - `winit_windows` - The [`WinitWindows`] resource
/// - `window_created_event` - The event writer for [`WindowCreatedEvent`] events
/// - `event_loop` - The event loop window target for creating windows
///
/// # Notes
/// This function is called in the event loop to create any new windows that were added.
/// It is also called at the start of the event loop to create any windows that were added before the event loop started.
///
/// # Panics
/// - If the winit window creation fails
/// - If the display handle cannot be retrieved
/// - If the window handle cannot be retrieved
fn create_windows(
    mut commands: Commands,
    mut query: Query<(Entity, &mut Window), Added<Window>>,
    mut winit_windows: NonSendMut<WinitWindows>,
    mut window_created_event: EventWriter<WindowCreatedEvent>,
    event_loop: &ActiveEventLoop,
) {
    for (entity, mut window) in query.iter_mut() {
        // If the winit window already exists somehow, don't create another one
        if winit_windows.entity_to_window.contains_key(&entity) {
            continue;
        }

        let winit_window = winit_windows
            .create_window(event_loop, entity, window.as_ref())
            .unwrap_or_else(|err| {
                panic!("Failed to create window for entity {:?}: {err}", entity);
            });

        window
            .resolution
            .set_scale_factor(winit_window.scale_factor());

        let display_handle = winit_window.display_handle().unwrap_or_else(|err| {
            panic!(
                "Failed to get display handle for window {:?}: {err}",
                winit_window.id()
            );
        });
        let window_handle = winit_window.window_handle().unwrap_or_else(|err| {
            panic!(
                "Failed to get window handle for window {:?}: {err}",
                winit_window.id()
            );
        });

        commands.entity(entity).insert(RawHandleWrapper {
            display_handle: display_handle.as_raw(),
            window_handle: window_handle.as_raw(),
        });

        commands.entity(entity).insert(CachedWindow(window.clone()));

        window_created_event.send(WindowCreatedEvent {
            window_id: winit_window.id(),
        });
    }
}

/// The condition at which the event loop will quit
///
/// Used in the [`WindowPlugin`] to determine the exit behaviour of the event loop.
#[allow(dead_code)]
#[derive(Default)]
pub enum ExitCondition {
    /// Quit when the primary window is closed
    OnPrimaryClosed,
    /// Quit when all windows are closed
    #[default]
    OnAllClosed,
    /// Don't quit no matter what
    DontExit,
}
