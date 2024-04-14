//! Bevy systems for the graphics module.

use crate::core::graphics::resources::{ExtractedWindows, GraphicsState};
use bevy_ecs::prelude::Res;
use bevy_ecs::system::{NonSend, ResMut};
use log::info;
use std::ops::DerefMut;
use winit::dpi::PhysicalSize;

#[cfg(any(target_os = "macos", target_os = "ios"))]
use crate::core::graphics::resources::NonSendMarker;

// TODO: Update documentation

/// Creates a surface for each window created.
///
/// Runs on `Prepare`
/// use the [`GraphicsState`] to create a surface for the window, passing in the window and raw handle.
// * Fun Fact: Regarding error handling, I eventually settled on only panicking in systems and never panic in helper functions.
// *           I don't know why I did that since no matter where it panics, if it does panic the program will crash.
pub fn rp_create_surface(
    #[cfg(any(target_os = "macos", target_os = "ios"))] _non_send: Option<NonSend<NonSendMarker>>,
    mut graphics_state: ResMut<GraphicsState<'static>>,
    extracted_windows: Res<ExtractedWindows>,
) {
    for (entity, window) in extracted_windows.windows.iter() {
        if !graphics_state.surface_states.contains_key(entity) {
            graphics_state
                .create_surface(window, *entity, &window.raw_handles)
                .unwrap_or_else(|err| {
                    panic!(
                        "Failed to create surface for window on {:?} with error {err}",
                        entity
                    )
                });

            info!("Surface created for window on {:?}", entity);
        }
    }
}

/// Resizes the surface for each window that has a resized event.
///
/// Runs on `Update` when a [`WindowResizedEvent`] is received,
pub fn rp_resize(
    #[cfg(any(target_os = "macos", target_os = "ios"))] _non_send: Option<NonSend<NonSendMarker>>,
    extracted_windows: Res<ExtractedWindows>,
    mut graphics_state: ResMut<GraphicsState<'static>>,
) {
    for (entity, window) in extracted_windows.windows.iter() {
        if !window.size_changed {
            continue;
        }

        let graphics_state = graphics_state.deref_mut();
        if let Some(surface_state) = graphics_state.surface_states.get_mut(entity) {
            surface_state.resize(
                PhysicalSize::new(window.physical_width, window.physical_height),
                &graphics_state.device,
            );
        }
    }
}
